//! SSE 过程流（CF Workers 版）。
//!
//! lug 的 SSE 是**预生成事件回放**（prefix → step×N → final → [DONE]），
//! 原实现用 axum `Sse` + `KeepAlive`；但 axum 0.8 的 sse 模块 feature-gate
//! 在 `tokio`，KeepAlive 内部 `tokio::time::sleep` 在 Workers 上无 reactor
//! 会 panic（见 lug-migration/notes/workers-rs-tech-notes.md §2）。
//! 此处改为手写：事件拼 `data: {json}\n\n` 帧（本模块纯函数，host 可测），
//! wasm 侧经 [`sse_axum_response`]（`worker::Body::from_stream`）变 JS
//! ReadableStream 回放，零延迟即发即收（§4.2：事件即算即发）。
//!
//! T7.5 接线：事件构造直接消费真 [`lug_engine::EvalOutput`]（lug_engine_shim
//! 已退役）；`num_to_json` / `usage_json` 与 lug-server 同源（routes.rs）。

use serde_json::{json, Value};

use crate::routes::{num_to_json, usage_json};

// ------------------------------------------------------------------ 帧序列化

/// 单个事件 → SSE 帧：`data: <payload>\n\n`。
pub fn sse_frame(payload: &str) -> String {
    format!("data: {payload}\n\n")
}

/// 已序列化的 serde_json::Value 事件序列 → SSE 帧序列
/// （逐帧 `data: {json}\n\n`；wire 格式与 axum `Sse` 逐字节一致——
/// golden 对照测试（tests/golden_vs_server.rs）以 lug-server 实测输出背书）。
pub fn frames_from_values(events: Vec<Value>) -> Vec<String> {
    events
        .into_iter()
        .map(|ev| sse_frame(&ev.to_string()))
        .collect()
}

/// 计算过程流的事件序列构造（与 lug-server `sse_stream` 逐字段对齐）。
/// `expr`：原始表达式；`output`：引擎求值输出（steps 预生成）。
/// 序列：prefix → step×N（cost_tokens 恒为 1，D2）→ final → [DONE]。
pub fn build_events(expr: &str, output: &lug_engine::EvalOutput) -> Vec<String> {
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
            "result": num_to_json(&st.result),
            "state": st.state,
        }));
    }

    events.push(json!({
        "type": "final",
        "expression": expr,
        "result": num_to_json(&output.result),
        "steps_count": output.steps.len(),
        "usage": usage_json(&output.usage),
    }));

    let mut frames = frames_from_values(events);
    frames.push(sse_frame("[DONE]"));
    frames
}

// ------------------------------------------------------------------ 响应构造

/// SSE 回放响应：预生成帧 → `worker::Body::from_stream` → ReadableStream。
/// 头部自设：from_stream 不自动补 Content-Type。
/// 原 entry.rs 胶水下移到本模块（worker 0.9 host 亦可编译，见 crate Cargo.toml
/// 注释）——axum handler 的 `process=true` 分支与 host 测试共用。
pub fn sse_response(frames: Vec<String>) -> axum::http::Response<worker::Body> {
    let stream = futures::stream::iter(
        frames
            .into_iter()
            .map(|frame| Ok::<_, std::convert::Infallible>(frame.into_bytes())),
    );
    let body = worker::Body::from_stream(stream).expect("infallible stream conversion");
    axum::http::Response::builder()
        .status(200)
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-store")
        .body(body)
        .expect("static SSE response parts are valid")
}

/// axum handler 直用版：`axum::http::Response<worker::Body>` → axum Response
/// （保状态码/头，body 经 `axum::body::Body::new` 适配 http_body trait）。
pub fn sse_axum_response(frames: Vec<String>) -> axum::response::Response {
    sse_response(frames).map(axum::body::Body::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lug_engine::Value;

    /// 真引擎求值（lug-engine 已是真依赖）："1+2" → 单步。
    fn real_output(expr: &str) -> lug_engine::EvalOutput {
        lug_engine::evaluate(expr).expect("golden test expression must evaluate")
    }

    #[test]
    fn frame_format_matches_sse_wire() {
        assert_eq!(sse_frame("[DONE]"), "data: [DONE]\n\n");
        assert_eq!(sse_frame("{\"a\":1}"), "data: {\"a\":1}\n\n");
    }

    #[test]
    fn event_sequence_is_prefix_steps_final_done() {
        let output = real_output("1+2");
        let frames = build_events("1+2", &output);
        assert_eq!(frames.len(), 4); // prefix + 1 step + final + [DONE]

        assert!(frames[0].starts_with("data: {\"type\":\"prefix\""));
        assert!(frames[0].contains("\"prefix\":\"+ 1 2\""));

        assert!(frames[1].starts_with("data: {\"type\":\"step\""));
        assert!(frames[1].contains("\"cost_tokens\":1"));
        assert!(frames[1].contains("\"operator\":\"+\""));

        assert!(frames[2].starts_with("data: {\"type\":\"final\""));
        assert!(frames[2].contains("\"expression\":\"1+2\""));
        assert!(frames[2].contains("\"steps_count\":1"));
        // 计费口径（D2）：prompt=词法 token 数（"1+2"→3），reasoning=步数，答案+1
        assert!(frames[2].contains("\"prompt_tokens\":3"));
        assert!(frames[2].contains("\"total_tokens\":5"));

        assert_eq!(frames[3], "data: [DONE]\n\n");
    }

    #[test]
    fn frames_from_values_wrap_each_payload() {
        let frames = frames_from_values(vec![
            json!({"type": "prefix", "prefix": "x"}),
            json!({"n": 1}),
        ]);
        assert_eq!(frames.len(), 2);
        assert_eq!(
            frames[0],
            "data: {\"type\":\"prefix\",\"prefix\":\"x\"}\n\n"
        );
        assert_eq!(frames[1], "data: {\"n\":1}\n\n");
    }

    #[test]
    fn empty_steps_stream_is_prefix_final_done() {
        // 空步骤流：常量表达式（如 "1"）不产生化简步骤
        let output = real_output("42");
        assert!(output.steps.is_empty(), "constant expression has no steps");
        let frames = build_events("42", &output);
        assert_eq!(frames.len(), 3); // prefix + final(steps_count:0) + [DONE]
        assert!(frames[0].contains("\"type\":\"prefix\""));
        assert!(frames[1].contains("\"type\":\"final\""));
        assert!(frames[1].contains("\"steps_count\":0"));
        assert_eq!(frames[2], "data: [DONE]\n\n");
    }

    #[test]
    fn multi_step_stream_counts_frames() {
        let output = real_output("1+2*3");
        assert_eq!(output.steps.len(), 2);
        let frames = build_events("1+2*3", &output);
        assert_eq!(frames.len(), 5); // prefix + 2 steps + final + [DONE]
        assert!(frames[1].contains("\"step\":1"));
        assert!(frames[2].contains("\"step\":2"));
    }

    /// Int/Big/Float 三态数值在 step/final 事件中的 JSON 形态
    /// （Big/Float 经十进制字面量，与 Python json.dumps 逐字节一致）。
    #[test]
    fn num_to_json_covers_value_triad() {
        assert_eq!(num_to_json(&Value::Int(7)), json!(7));
        // Big：真引擎溢出晋升（2**100 > i64::MAX）
        let big_out = real_output("2**100");
        assert!(matches!(big_out.result, Value::Big(_)));
        assert_eq!(
            num_to_json(&big_out.result),
            json!(1267650600228229401496703205376u128)
        );
        assert_eq!(num_to_json(&Value::Float(0.25)), json!(0.25));
        // 整值浮点整数化（8.0 → 8）
        assert_eq!(num_to_json(&Value::Float(8.0)), json!(8));
    }

    #[test]
    fn usage_json_field_order_and_details() {
        let u = lug_engine::Usage {
            prompt_tokens: 3,
            completion_tokens: 2,
            total_tokens: 5,
            reasoning_tokens: 1,
        };
        let v = usage_json(&u);
        let keys: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        assert_eq!(
            keys,
            vec![
                "prompt_tokens",
                "completion_tokens",
                "total_tokens",
                "completion_tokens_details",
            ]
        );
        assert_eq!(v["completion_tokens_details"]["reasoning_tokens"], 1);
    }

    /// `sse_response` 的头部装配（Content-Type/Cache-Control）无法在 host 单测：
    /// `Body::from_stream` 构造即触达 wasm-bindgen FFI（host 桩 panic，同
    /// token_urlsafe 的先例）——头部常量在 wasm check 编译面覆盖，运行时行为
    /// 留 wrangler dev/部署冒烟。此处只验证帧拼接层的纯函数面。
    #[test]
    fn sse_frames_concatenate_in_order() {
        let output = real_output("2**100");
        let frames = build_events("2**100", &output);
        let body: String = frames.concat();
        assert!(body.starts_with("data: {\"type\":\"prefix\""));
        assert!(body.ends_with("data: [DONE]\n\n"));
        // Big 结果以十进制字面量进 final 事件（arbitrary_precision 保真）
        assert!(body.contains("\"result\":1267650600228229401496703205376"));
    }
}
