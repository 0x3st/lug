//! 词法分析：表达式字符串 → token 序列。
//!
//! 与 Python 原型逐字节对齐（含错误信息），语义增强（v1-alpha.2，L1 规则层）：
//! D1 移除 `x`/`X` 乘号别名；算子词别名（`加` `乘以` `plus` `整除`…，最长匹配）
//! 与中文数字（`三十五` `三点五` `十万`、ASCII 续接 `3万5千`）接词法；
//! Unicode 运算别名 `×` `÷` `^` 保留。

use crate::error::ExprError;
use crate::semantic;
use crate::value::Value;
use num_bigint::BigInt;

/// 词法 token。`Op` 存规范符号（别名 `×`→`*`、`÷`→`/`、`^`→`**`、
/// `加`→`+` 等在此归一），渲染/步骤状态均使用规范形。
#[derive(Clone, Debug)]
pub enum Token {
    /// 数字字面量（Int 溢出自动晋升 Big，含中文数字解析产物与常量折叠）。
    Num(Value),
    /// 规范二元运算符符号。
    Op(&'static str),
    /// 括号 `(` 或 `)`。
    Paren(char),
    /// 标识符：变量（sum/prod 绑定）或未知名（求值期报 undefined）。
    /// 常量（pi/e/tau）与算子词别名不进入此变体（词法期即折叠/归一）。
    Ident(String),
}

pub fn tokenize(s: &str) -> Result<Vec<Token>, ExprError> {
    if s.trim().is_empty() {
        return Err(ExprError::expression("Expression must not be empty"));
    }
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let mut tokens = Vec::new();
    // 上一 token 的结束下标（用于"紧贴续接"判定：3万 中 万 紧跟 3）
    let mut prev_end = usize::MAX;
    let mut i = 0usize;

    while i < n {
        let c = chars[i];

        // 1. 空白
        if c.is_whitespace() {
            i += 1;
            continue;
        }

        // 2. ASCII 数字/小数点字面量
        if c.is_ascii_digit() || c == '.' {
            // T9a S5：进制前缀 0x/0b/0o（大小写均接受，与 Python 一致）
            if c == '0'
                && i + 1 < n
                && matches!(chars[i + 1], 'x' | 'X' | 'b' | 'B' | 'o' | 'O')
            {
                let (value, len) = lex_radix(&chars[i..])?;
                tokens.push(Token::Num(value));
                i += len;
                prev_end = i;
                continue;
            }
            let (value, len) = lex_number(&chars[i..])?;
            tokens.push(Token::Num(value));
            i += len;
            prev_end = i;
            continue;
        }

        // 3. 数字续接（万/亿/千 紧贴前一个整数值：3万5千 = 35000）
        if i == prev_end
            && matches!(c, '万' | '亿' | '千')
            && matches!(
                tokens.last(),
                Some(Token::Num(Value::Int(_) | Value::Big(_)))
            )
        {
            let prev = match tokens.last() {
                Some(Token::Num(v)) => v.clone(),
                _ => unreachable!("上方已判定为数字"),
            };
            match semantic::continue_cn_number(&chars[i..], &prev) {
                Some((value, len)) => {
                    *tokens.last_mut().unwrap() = Token::Num(value);
                    i += len;
                    prev_end = i;
                    continue;
                }
                None => { /* 非法序列 → 落入下方未识别路径 */ }
            }
        }

        // 4. 中文数字（三十五 / 一百零五 / 三点五 / 十万…）
        if let Some((value, len)) = semantic::parse_cn_number(&chars[i..], 0, None) {
            tokens.push(Token::Num(value));
            i += len;
            prev_end = i;
            continue;
        }

        // 5. 算子词别名（最长匹配：加/加上/乘/乘以/乘方/整除/plus/modulo…）
        if let Some((sym, len)) = semantic::match_word_alias(&chars, i) {
            tokens.push(Token::Op(sym));
            i += len;
            prev_end = i;
            continue;
        }

        // 5.5 阶乘后缀
        if c == '!' {
            tokens.push(Token::Op("!"));
            i += 1;
            prev_end = i;
            continue;
        }

        // 5.6 标识符：字母/下划线开头，后可跟数字（log2/log10 才能成词）
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < n
                && (chars[i].is_ascii_alphabetic() || chars[i].is_ascii_digit() || chars[i] == '_')
            {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            prev_end = i;
            // ① 算子词别名（整词完全一致才归一）
            if let Some((_, sym)) = crate::registry::WORD_ALIASES
                .iter()
                .find(|(w, _)| *w == word)
            {
                tokens.push(Token::Op(sym));
                continue;
            }
            // ② 常量折叠为字面量（与数字字面量同待遇，双路一致）
            if let Some((_, cval)) = crate::registry::CONSTANTS
                .iter()
                .find(|(name, _)| *name == word)
            {
                tokens.push(Token::Num(Value::Float(*cval)));
                continue;
            }
            // ③ 标识符（变量）
            tokens.push(Token::Ident(word));
            continue;
        }

        // 6. 双字符运算符
        if i + 1 < n && c == '*' && chars[i + 1] == '*' {
            tokens.push(Token::Op("**"));
            i += 2;
            prev_end = i;
            continue;
        }
        if i + 1 < n && c == '/' && chars[i + 1] == '/' {
            tokens.push(Token::Op("//"));
            i += 2;
            prev_end = i;
            continue;
        }
        // T9a S4：移位双字符（先于单字符，孤立 < > 仍不可识别）
        if i + 1 < n && (c == '<' || c == '>') && chars[i + 1] == c {
            tokens.push(Token::Op(if c == '<' { "<<" } else { ">>" }));
            i += 2;
            prev_end = i;
            continue;
        }

        // 7. 单字符运算符、逗号（函数实参分隔）与 Unicode 别名
        let op = match c {
            '+' | '-' | '*' | '/' | '%' | '&' | '|' | '~' => Some(single_to_static(c)),
            ',' => Some(","),
            '负' => Some("-"), // 口语负号："负一百" = -100
            '×' => Some("*"),
            '÷' => Some("/"),
            '^' => Some("**"),
            _ => None,
        };
        if let Some(sym) = op {
            tokens.push(Token::Op(sym));
            i += 1;
            prev_end = i;
            continue;
        }

        // 8. 括号
        if c == '(' || c == ')' {
            tokens.push(Token::Paren(c));
            i += 1;
            prev_end = i;
            continue;
        }

        // 9. 未识别字符（与原型同文案；D1：x/X 落入此分支）
        return Err(ExprError::expression(format!(
            "Unrecognized character {:?} at position {i}; \
             only digits, operators, parentheses and spaces are allowed",
            c
        )));
    }
    if tokens.is_empty() {
        return Err(ExprError::expression("Expression contains no valid tokens"));
    }
    Ok(tokens)
}

/// 数字字面量：ASCII 数字与小数点。Int 溢出 → Big（Python int() 任意精度）；
/// 含点 → Float（Python float("3.") / float(".5") 行为一致）。
fn lex_number(chars: &[char]) -> Result<(Value, usize), ExprError> {
    let mut i = 0usize;
    while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
        i += 1;
    }
    let text: String = chars[..i].iter().collect();
    if text.matches('.').count() > 1 {
        return Err(ExprError::expression(format!(
            "Invalid number literal: {text}"
        )));
    }
    let value = if text.contains('.') {
        Value::Float(text.parse::<f64>().unwrap_or(0.0))
    } else {
        match text.parse::<i64>() {
            Ok(v) => Value::Int(v),
            Err(_) => {
                Value::Big(BigInt::parse_bytes(text.as_bytes(), 10).expect("纯数字串必可解析"))
            }
        }
    };
    Ok((value, i))
}

/// 进制字面量：0x/0X 十六进制、0b/0B 二进制、0o/0O 八进制（T9a S5）。
/// 前缀后连续扫描字母数字 run，整 run 校验：任一位非法或 run 为空 →
/// "Invalid number literal: {0x+run}"（与 1.2.3 的既有文案同族）。
/// Int 放不下自动晋升 Big（BigInt::parse_bytes）。
fn lex_radix(chars: &[char]) -> Result<(Value, usize), ExprError> {
    let radix = match chars[1] {
        'x' | 'X' => 16u32,
        'b' | 'B' => 2,
        'o' | 'O' => 8,
        _ => unreachable!("调用方已判定前缀"),
    };
    let mut i = 2usize;
    while i < chars.len() && chars[i].is_ascii_alphanumeric() {
        i += 1;
    }
    let digits: String = chars[2..i].iter().collect();
    let text: String = chars[..i].iter().collect();
    if digits.is_empty() || digits.chars().any(|d| d.to_digit(radix).is_none()) {
        return Err(ExprError::expression(format!(
            "Invalid number literal: {text}"
        )));
    }
    let value = match i64::from_str_radix(&digits, radix) {
        Ok(v) => Value::Int(v),
        Err(_) => Value::Big(
            BigInt::parse_bytes(digits.as_bytes(), radix).expect("已验证位必可解析"),
        ),
    };
    Ok((value, i))
}

fn single_to_static(c: char) -> &'static str {
    match c {
        '+' => "+",
        '-' => "-",
        '*' => "*",
        '/' => "/",
        '%' => "%",
        '&' => "&",
        '|' => "|",
        '~' => "~",
        _ => unreachable!("调用方已保证字符合法"),
    }
}
