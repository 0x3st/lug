//! SSE 过程流（CF Workers 版）。
//!
//! lug 的 SSE 是**预生成事件回放**（prefix → step×N → final → [DONE]），
//! 原实现用 axum `Sse` + `KeepAlive`；但 axum 0.8 的 sse 模块 feature-gate
//! 在 `tokio`，KeepAlive 内部 `tokio::time::sleep` 在 Workers 上无 reactor
//! 会 panic（见 lug-migration/notes/workers-rs-tech-notes.md §2）。
//! 此处改为手写：事件拼 `data: {json}\n\n` 帧（本模块纯函数，host 可测），
//! wasm 侧经 `entry::sse_response`（`worker::Body::from_stream`）变 JS
//! ReadableStream 回放，零延迟即发即收（§4.2：事件即算即发）。

use serde_json::{json, Value};

/// 单个事件 → SSE 帧：`data: <payload>\n\n`。
pub fn sse_frame(payload: &str) -> String {
    format!("data: {payload}\n\n")
}

/// 已序列化的 serde_json::Value 事件序列 → SSE 帧序列
/// （逐帧 `data: {json}\n\n`；wire 格式与 axum `Sse` 逐字节一致）。
pub fn frames_from_values(events: Vec<Value>) -> Vec<String> {
    events
        .into_iter()
        .map(|ev| sse_frame(&ev.to_string()))
        .collect()
}

/// 计算过程流的事件序列构造（与 lug-server `sse_stream` 逐字段对齐）。
/// `expr`：原始表达式；`output`：引擎求值输出（steps 预生成）。
/// 序列：prefix → step×N（cost_tokens 恒为 1，D2）→ final → [DONE]。
pub fn build_events(expr: &str, output: &lug_engine_shim::EvalOutput) -> Vec<String> {
    let mut events = Vec::with_capacity(output.steps.len() + 2);

    events.push(json!({"type": "prefix", "prefix": output.prefix_display}));

    for st in &output.steps {
        events.push(json!({
            "type": "step",
            "cost_tokens": 1,
            "step": st.step,
            "expr": st.expr,
            "operator": st.operator,
            "operator_code": st.operator_code,
            "result": lug_engine_shim::num_to_json(&st.result),
            "state": st.state,
        }));
    }

    events.push(json!({
        "type": "final",
        "expression": expr,
        "result": lug_engine_shim::num_to_json(&output.result),
        "steps_count": output.steps.len(),
        "usage": lug_engine_shim::usage_json(&output.usage),
    }));

    let mut frames = frames_from_values(events);
    frames.push(sse_frame("[DONE]"));
    frames
}

/// 兼容垫片：骨架阶段 lug-engine 尚未接入（T7 接线），
/// 事件结构以最小类型先行，保证 SSE 层可独立测试。
pub mod lug_engine_shim {
    use serde_json::{json, Value};

    #[derive(Debug, Clone)]
    pub struct Step {
        pub step: u64,
        pub expr: String,
        pub operator: String,
        pub operator_code: String,
        pub result: Value,
        pub state: Vec<Value>,
    }

    #[derive(Debug, Clone)]
    pub struct Usage {
        pub prompt_tokens: u64,
        pub completion_tokens: u64,
        pub total_tokens: u64,
        pub reasoning_tokens: u64,
    }

    #[derive(Debug, Clone)]
    pub struct EvalOutput {
        pub prefix_display: String,
        pub steps: Vec<Step>,
        pub result: Value,
        pub usage: Usage,
    }

    /// 引擎数值 → JSON（T7 换成 lug-server::num_to_json 的平移实现，
    /// 语义：Big/Float 十进制字面量、2.0/0.0→0 双格式，与 Python 逐字节一致）。
    pub fn num_to_json(v: &Value) -> Value {
        v.clone()
    }

    pub fn usage_json(u: &Usage) -> Value {
        json!({
            "prompt_tokens": u.prompt_tokens,
            "completion_tokens": u.completion_tokens,
            "total_tokens": u.total_tokens,
            "completion_tokens_details": {"reasoning_tokens": u.reasoning_tokens},
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lug_engine_shim::{EvalOutput, Step, Usage};

    fn sample_output() -> EvalOutput {
        EvalOutput {
            prefix_display: "+ 1 2".into(),
            steps: vec![Step {
                step: 1,
                expr: "1 2".into(),
                operator: "+".into(),
                operator_code: "ADD".into(),
                result: json!(3),
                state: vec![json!(3)],
            }],
            result: json!(3),
            usage: Usage {
                prompt_tokens: 3,
                completion_tokens: 1,
                total_tokens: 4,
                reasoning_tokens: 0,
            },
        }
    }

    #[test]
    fn frame_format_matches_sse_wire() {
        assert_eq!(sse_frame("[DONE]"), "data: [DONE]\n\n");
        assert_eq!(sse_frame("{\"a\":1}"), "data: {\"a\":1}\n\n");
    }

    #[test]
    fn event_sequence_is_prefix_steps_final_done() {
        let frames = build_events("1+2", &sample_output());
        assert_eq!(frames.len(), 4); // prefix + 1 step + final + [DONE]

        assert!(frames[0].starts_with("data: {\"type\":\"prefix\""));
        assert!(frames[0].contains("\"prefix\":\"+ 1 2\""));

        assert!(frames[1].starts_with("data: {\"type\":\"step\""));
        assert!(frames[1].contains("\"cost_tokens\":1"));
        assert!(frames[1].contains("\"operator_code\":\"ADD\""));

        assert!(frames[2].starts_with("data: {\"type\":\"final\""));
        assert!(frames[2].contains("\"expression\":\"1+2\""));
        assert!(frames[2].contains("\"steps_count\":1"));
        assert!(frames[2].contains("\"prompt_tokens\":3"));

        assert_eq!(frames[3], "data: [DONE]\n\n");
    }

    #[test]
    fn frames_from_values_wrap_each_payload() {
        let frames = frames_from_values(vec![
            json!({"type": "prefix", "prefix": "x"}),
            json!({"n": 1}),
        ]);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0], "data: {\"type\":\"prefix\",\"prefix\":\"x\"}\n\n");
        assert_eq!(frames[1], "data: {\"n\":1}\n\n");
    }

    #[test]
    fn empty_steps_stream_is_prefix_final_done() {
        let mut out = sample_output();
        out.steps.clear();
        let frames = build_events("1+2", &out);
        assert_eq!(frames.len(), 3); // prefix + final(steps_count:0) + [DONE]
        assert!(frames[0].contains("\"type\":\"prefix\""));
        assert!(frames[1].contains("\"type\":\"final\""));
        assert!(frames[1].contains("\"steps_count\":0"));
        assert_eq!(frames[2], "data: [DONE]\n\n");
    }

    #[test]
    fn multi_step_stream_counts_frames() {
        let mut out = sample_output();
        out.steps.push(Step {
            step: 2,
            expr: "3".into(),
            operator: "*".into(),
            operator_code: "MUL".into(),
            result: json!(9),
            state: vec![json!(9)],
        });
        let frames = build_events("1+2*3", &out);
        assert_eq!(frames.len(), 5); // prefix + 2 steps + final + [DONE]
        assert!(frames[2].contains("\"step\":2"));
    }
}
