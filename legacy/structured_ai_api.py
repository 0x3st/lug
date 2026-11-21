#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
结构化 AI API —— 模拟当前主流 AI 接口（OpenAI 风格）

能力：
  1. 接收表达式字符串（唯一输入形态），按词法拆分 tokens 计费
  2. 解析成 AST 并编译为先序（preorder/波兰式）中间表示 —— 可直接栈式计算
  3. 后序遍历逐步求值，每步记录（子表达式/运算符编码/结果/剩余表达式）
  4. "process": true 开启运算过程 → SSE 流式输出（prefix → step → final）；
     不开启 → 直接输出答案

接口（运算接口另有平台登录/密钥管理与静态页面路由，未注册路径返回 404）：
  - POST /f/a/o/calculate   表达式求值（默认直接返回答案；process=true 走 SSE 事件流）
  - GET  /f/a/o/operator    运算符编码表
  - Authorization: Bearer sk-xxx 鉴权；OpenAI 风格错误结构（错误信息为英文）

计费口径：prompt_tokens = 表达式拆分出的 token 数；
          reasoning_tokens = 各步骤子表达式 token 数之和（仅开启运算过程时产生）；
          completion_tokens = reasoning_tokens + 1（最终答案）。

零依赖，仅用 Python 标准库。启动：python3 structured_ai_api.py
"""

import hashlib
import hmac
import json
import math
import os
import secrets
import threading
import time
import uuid
from http.cookies import SimpleCookie, CookieError
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlsplit
from platform_auth import (
    AuthConfig, GitHubOAuth, OAuthError, SessionStore, owner_id,
    CALLBACK_PATH, SESSION_COOKIE, STATE_COOKIE, STATE_TTL,
)

HOST = os.environ.get("HOST", "127.0.0.1")
PORT = int(os.environ.get("PORT", "8000"))
MODEL_ID = "calculator-1"
AUTH = AuthConfig()
OAUTH = GitHubOAuth(AUTH)
SESSION_TTL = 7 * 24 * 3600
WEB_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "web")

# ---------------------------------------------------------------- 运算符定义 --

def _add(a, b): return a + b
def _sub(a, b): return a - b
def _mul(a, b): return a * b
def _div(a, b): return a / b
def _floordiv(a, b): return a // b
def _mod(a, b): return a % b
def _pow(a, b): return a ** b

OPERATORS = {
    "+":  (_add,      "加法"),
    "-":  (_sub,      "减法"),
    "*":  (_mul,      "乘法"),
    "×":  (_mul,      "乘法"),
    "x":  (_mul,      "乘法"),
    "/":  (_div,      "除法"),
    "÷":  (_div,      "除法"),
    "//": (_floordiv, "整除"),
    "%":  (_mod,      "取模"),
    "**": (_pow,      "幂运算"),
    "^":  (_pow,      "幂运算"),
}

# 中英文别名 -> 标准符号
OP_ALIASES = {
    "add": "+", "plus": "+", "sum": "+", "加": "+", "加上": "+",
    "sub": "-", "minus": "-", "subtract": "-", "减": "-", "减去": "-",
    "mul": "*", "times": "*", "multiply": "*", "乘": "*", "乘以": "*",
    "div": "/", "divide": "/", "除": "/", "除以": "/",
    "floordiv": "//", "整除": "//",
    "mod": "%", "modulo": "%", "取余": "%", "取模": "%",
    "pow": "**", "power": "**", "幂": "**", "乘方": "**",
}

# --------------------------------------------- 运算符编码（编码化协议）--
# 规范编码：0=+  1=-  2=*  3=/  4=//  5=%  6=**
CANONICAL_OPS = ["+", "-", "*", "/", "//", "%", "**"]
OPERATOR_CODES = {code: sym for code, sym in enumerate(CANONICAL_OPS)}   # 编码 -> 符号
SYMBOL_TO_CODE = {sym: code for code, sym in OPERATOR_CODES.items()}     # 符号 -> 编码
CODE_TABLE_HINT = " ".join(f"{c}={s}" for c, s in sorted(OPERATOR_CODES.items()))


def normalize_operator(op):
    """把运算符归一化成标准符号，返回 (符号, 描述) 或 (None, None)。
    接受三种写法：编码(int)、编码字符串("3")、符号/别名("+"/"plus"/"加")。
    """
    sym = None
    if isinstance(op, bool):
        return None, None
    if isinstance(op, (int, float)):          # 数字编码，如 0/1/2...
        code = int(op)
        if op == code and code in OPERATOR_CODES:
            sym = OPERATOR_CODES[code]
    elif isinstance(op, str):
        key = op.strip().lower()
        if key.isdigit():                      # 编码字符串，如 "3"
            code = int(key)
            if code in OPERATOR_CODES:
                sym = OPERATOR_CODES[code]
        elif key in OPERATORS:                 # 符号，如 "+"
            sym = key
        elif key in OP_ALIASES:                # 别名，如 "加"
            sym = OP_ALIASES[key]
    if sym is None:
        return None, None
    return sym, OPERATORS[sym][1]


def format_num(x):
    """整数化的浮点显示为 int，避免 8.0 这种输出。"""
    if isinstance(x, float):
        if math.isfinite(x) and x.is_integer():
            return int(x)
        return round(x, 12)
    return x


def compute(a, sym, b):
    """执行计算，返回 (结果, 错误信息)。"""
    if sym == "/" and b == 0:
        return None, "Division by zero"
    if sym == "//" and b == 0:
        return None, "Floor division by zero"
    if sym == "%" and b == 0:
        return None, "Modulo by zero"
    if sym == "**" and abs(b) > 10000:
        return None, "Exponent too large (|b| > 10000)"
    try:
        return format_num(OPERATORS[sym][0](a, b)), None
    except OverflowError:
        return None, "Numeric overflow"
    except Exception as e:  # pragma: no cover
        return None, f"Computation failed: {e}"


# ================================================== 运算处理器（表达式引擎）==
# 流水线：表达式字符串 --tokenize--> tokens --parse--> AST
#         --ast_to_prefix--> 先序 IR（波兰式，运算符为编码，可栈式计算）
#         --evaluate_tree--> 后序遍历逐步求值（记录每一步化简）

class ExprError(Exception):
    """表达式解析/求值错误。异常项直接携带响应用的 code：
    'expression_error'（语法）| 'computation_error'（计算）。"""

    def __init__(self, message, code="expression_error"):
        super().__init__(message)
        self.code = code


def tokenize_expr(s):
    """表达式字符串 -> token 列表。token 形如 ('num', v) / ('op', sym) / ('paren', '(' 或 ')')"""
    if not isinstance(s, str) or not s.strip():
        raise ExprError("Expression must not be empty")
    tokens, i, n = [], 0, len(s)
    while i < n:
        c = s[i]
        if c.isspace():
            i += 1
        elif c.isdigit() or c == ".":
            j = i
            while j < n and (s[j].isdigit() or s[j] == "."):
                j += 1
            text = s[i:j]
            if text.count(".") > 1:
                raise ExprError(f"Invalid number literal: {text}")
            tokens.append(("num", float(text) if "." in text else int(text)))
            i = j
        elif s[i:i + 2] in ("**", "//"):
            tokens.append(("op", s[i:i + 2]))
            i += 2
        elif c in "+-*/%":
            tokens.append(("op", c))
            i += 1
        elif c in "×xX":
            tokens.append(("op", "*"))
            i += 1
        elif c == "÷":
            tokens.append(("op", "/"))
            i += 1
        elif c == "^":
            tokens.append(("op", "**"))
            i += 1
        elif c in "()":
            tokens.append(("paren", c))
            i += 1
        else:
            raise ExprError(f"Unrecognized character {c!r} at position {i}; "
                            "only digits, operators, parentheses and spaces are allowed")
    if not tokens:
        raise ExprError("Expression contains no valid tokens")
    return tokens


# ---- 递归下降解析（与 Python 优先级一致，** 右结合）：----
#   add    := mul  (("+" | "-") mul)*
#   mul    := unary (("*" | "/" | "//" | "%") unary)*
#   unary  := ("+" | "-") unary | power
#   power  := primary ("**" unary)?          # 右结合，指数可带符号
#   primary:= NUMBER | "(" add ")"

class NumNode:
    def __init__(self, value, span):
        self.value, self.span = value, span


class UnaryNode:
    def __init__(self, sign, child, span):
        self.sign, self.child, self.span = sign, child, span


class BinNode:
    def __init__(self, sym, left, right, span):
        self.sym, self.left, self.right, self.span = sym, left, right, span


def parse_tokens(tokens):
    pos = [0]

    def peek():
        return tokens[pos[0]] if pos[0] < len(tokens) else (None, None)

    def advance():
        tok = peek()
        pos[0] += 1
        return tok

    def parse_primary():
        kind, val = peek()
        if kind == "num":
            advance()
            return NumNode(val, (pos[0] - 1, pos[0]))
        if kind == "paren" and val == "(":
            start = pos[0]
            advance()
            node = parse_add()
            kind2, val2 = peek()
            if not (kind2 == "paren" and val2 == ")"):
                raise ExprError("Mismatched parentheses: missing ')'")
            advance()
            node.span = (start, pos[0])   # span 包含括号，化简时连同括号一起替换
            return node
        got = val if val is not None else "end of expression"
        raise ExprError(f"Syntax error: unexpected {got!r} at position {pos[0]}, "
                        "expected a number or '('")

    def parse_power():
        base = parse_primary()
        if peek() == ("op", "**"):
            advance()
            exp = parse_unary()           # 右结合，指数可带一元符号
            return BinNode("**", base, exp, (base.span[0], exp.span[1]))
        return base

    def parse_unary():
        kind, val = peek()
        if kind == "op" and val in ("+", "-"):
            start = pos[0]
            advance()
            child = parse_unary()
            return UnaryNode(1 if val == "+" else -1, child, (start, child.span[1]))
        return parse_power()

    def parse_mul():
        left = parse_unary()
        while peek()[0] == "op" and peek()[1] in ("*", "/", "//", "%"):
            sym = advance()[1]
            right = parse_unary()
            left = BinNode(sym, left, right, (left.span[0], right.span[1]))
        return left

    def parse_add():
        left = parse_mul()
        while peek()[0] == "op" and peek()[1] in ("+", "-"):
            sym = advance()[1]
            right = parse_mul()
            left = BinNode(sym, left, right, (left.span[0], right.span[1]))
        return left

    root = parse_add()
    if pos[0] != len(tokens):
        rest = " ".join(str(t[1]) for t in tokens[pos[0]:])
        raise ExprError(f"Unexpected tokens after the end of expression: {rest}")
    return root


# ---- 先序（preorder / 波兰式）中间表示：op 在前、操作数在后，可用栈直接计算 ----

def ast_to_prefix(node):
    """AST -> 先序序列。返回 (prefix_tokens, prefix_str)。
    prefix_tokens: [{"op": 编码} | {"num": v} | {"op": "neg"/"pos"}] —— 运算符编码化
    prefix_str:    可读波兰式，如 "+ 3 * 5 ** 2 + 1 1"
    """
    toks, syms = [], []

    def walk(n):
        if isinstance(n, NumNode):
            toks.append({"num": format_num(n.value)})
            syms.append(str(format_num(n.value)))
        elif isinstance(n, UnaryNode):
            toks.append({"op": "neg" if n.sign == -1 else "pos"})
            syms.append("-" if n.sign == -1 else "+")
            walk(n.child)
        else:
            toks.append({"op": SYMBOL_TO_CODE[n.sym]})
            syms.append(n.sym)
            walk(n.left)
            walk(n.right)

    walk(node)
    return toks, " ".join(syms)


def evaluate_prefix(prefix_tokens):
    """栈式求值先序 token 序列（从右往左扫：数字入栈，运算符弹栈计算）。"""
    stack = []
    for tok in reversed(prefix_tokens):
        if "num" in tok:
            stack.append(tok["num"])
            continue
        op = tok["op"]
        if op == "neg":
            stack.append(-stack.pop())
        elif op == "pos":
            pass
        else:
            sym = OPERATOR_CODES.get(op)
            if sym is None:
                raise ExprError(f"Unknown operator code in prefix IR: {op!r}")
            # 倒序扫描先序式：栈顶是左操作数（后算完的子树），底下是右操作数
            left = stack.pop()
            right = stack.pop()
            res, err = compute(left, sym, right)
            if err:
                raise ExprError(f"Prefix evaluation failed: {left} {sym} {right}: {err}",
                                code="computation_error")
            stack.append(res)
    if len(stack) != 1:
        raise ExprError("Invalid prefix token sequence")
    return stack[0]


def _render_tokens(tokens):
    """把 token 列表渲染回表达式文本（跳过已化简的占位 None）。"""
    parts = []
    for t in tokens:
        if t is None:
            continue
        parts.append(str(format_num(t[1])) if t[0] == "num" else str(t[1]))
    return "".join(parts)


def evaluate_tree(root, tokens):
    """后序遍历求值（即实际计算顺序），逐步化简并记录步骤。返回 (result, steps)。"""
    work = list(tokens)          # 工作区：已算完的子表达式被替换为数值 token
    steps = []

    def substitute(span, value):
        s, e = span
        work[s] = ("num", value)
        for idx in range(s + 1, e):
            work[idx] = None      # 占位，渲染时跳过

    def eval_node(node):
        if isinstance(node, NumNode):
            return node.value
        if isinstance(node, UnaryNode):
            return node.sign * eval_node(node.child)
        a = eval_node(node.left)
        b = eval_node(node.right)
        expr_text = _render_tokens(work[node.span[0]:node.span[1]])
        if expr_text.startswith("(") and expr_text.endswith(")"):
            expr_text = expr_text[1:-1]   # 展示时去掉最外层括号
        res, err = compute(a, node.sym, b)
        if err:
            raise ExprError(f"Step {len(steps) + 1} failed: {expr_text}: {err}",
                            code="computation_error")
        substitute(node.span, res)
        steps.append({
            "step": len(steps) + 1,
            "expr": expr_text,
            "operator": node.sym,
            "operator_code": SYMBOL_TO_CODE[node.sym],
            "result": format_num(res),
            "state": _render_tokens(work),
        })
        return res

    result = eval_node(root)
    return result, steps


def evaluate_full(text):
    """完整流水线：解析 -> 先序 IR -> AST 逐步求值 -> 先序栈式验算（双路一致性校验）。
    返回 (result, steps, prefix_tokens, prefix_str)。
    """
    tokens = tokenize_expr(text)
    root = parse_tokens(tokens)
    prefix_tokens, prefix_str = ast_to_prefix(root)
    result, steps = evaluate_tree(root, tokens)
    check = evaluate_prefix(prefix_tokens)   # 用先序 IR 再算一遍，交叉验证
    if format_num(check) != format_num(result):
        raise ExprError(f"Internal consistency check failed: AST={result}, prefix={check}",
                        code="computation_error")
    return result, steps, prefix_tokens, prefix_str


def extract_expression(body):
    """从请求体提取表达式字符串（唯一支持的输入形态），返回 (expr, err)。"""
    expr = body.get("expression") if isinstance(body, dict) else None
    if not isinstance(expr, str) or not expr.strip():
        return None, 'Input must be an expression string, e.g. {"expression": "3+5*2**(1+1)"}'
    return expr.strip(), None


# ----------------------------------------------------------------- 响应构造 --

def count_tokens(expr):
    """计费口径：表达式按词法拆分后的 token 数。"""
    try:
        return len(tokenize_expr(expr))
    except ExprError:
        return 0


def build_usage(expr, steps):
    """按 tokens 计费：prompt=表达式 token 数；reasoning=各步子表达式 token 数之和；
    completion=reasoning+1（最终结果数字）。"""
    prompt = count_tokens(expr)
    reasoning = sum(count_tokens(st["expr"]) for st in steps)
    completion = reasoning + 1
    return {
        "prompt_tokens": prompt,
        "completion_tokens": completion,
        "total_tokens": prompt + completion,
        "completion_tokens_details": {"reasoning_tokens": reasoning},
    }


def api_error(message, err_type="invalid_request_error", code=None, status=400):
    """OpenAI 风格的错误结构。"""
    return status, {
        "error": {"message": message, "type": err_type, "param": None, "code": code}
    }


# ------------------------------------------------ API Key 存储与平台会话 --

class KeyStore:
    """API Key 存储（JSON 文件持久化）。明文只在创建时返回一次，落库存 SHA-256。"""

    def __init__(self, path, legacy_owner_id=""):
        self.path = path
        self.lock = threading.Lock()
        self.data = {"keys": {}}
        if os.path.exists(path):
            try:
                with open(path, encoding="utf-8") as f:
                    self.data = json.load(f)
            except (json.JSONDecodeError, OSError) as exc:
                raise RuntimeError("Cannot read API key store; refusing to overwrite it.") from exc
        # Explicit, one-time migration only. Never give old keys to the first login.
        if legacy_owner_id:
            changed = False
            for rec in self.data["keys"].values():
                if "owner_id" not in rec:
                    rec["owner_id"] = "github:" + legacy_owner_id
                    changed = True
            if changed:
                self._save()

    def _save(self):
        tmp = self.path + ".tmp"
        fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            os.fchmod(f.fileno(), 0o600)
            json.dump(self.data, f, ensure_ascii=False, indent=2)
        os.replace(tmp, self.path)

    @staticmethod
    def _hash(raw):
        return hashlib.sha256(raw.encode()).hexdigest()

    def create(self, name, owner_id):
        raw = "sk-" + secrets.token_urlsafe(36)
        kid = uuid.uuid4().hex[:12]
        rec = {
            "id": kid,
            "owner_id": owner_id,
            "name": str(name or "default")[:40],
            "key_hash": self._hash(raw),
            "prefix": raw[:11] + "..." + raw[-4:],   # 列表展示用，不含完整密钥
            "created_at": int(time.time()),
            "status": "active",
            "last_used": None,
            "usage": {"requests": 0, "prompt_tokens": 0,
                      "completion_tokens": 0, "total_tokens": 0},
        }
        with self.lock:
            self.data["keys"][kid] = rec
            self._save()
        return raw, rec

    def validate(self, raw):
        """校验明文 key，命中且 active 返回记录，否则 None。"""
        if not raw or not raw.startswith("sk-"):
            return None
        h = self._hash(raw)
        with self.lock:
            for rec in self.data["keys"].values():
                if rec["key_hash"] == h and rec["status"] == "active":
                    return rec
        return None

    def record_usage(self, kid, prompt, completion):
        with self.lock:
            rec = self.data["keys"].get(kid)
            if not rec:
                return
            u = rec["usage"]
            u["requests"] += 1
            u["prompt_tokens"] += prompt
            u["completion_tokens"] += completion
            u["total_tokens"] += prompt + completion
            rec["last_used"] = int(time.time())
            self._save()

    def list(self, owner_id):
        with self.lock:
            out = []
            for rec in self.data["keys"].values():
                if rec.get("owner_id", "local") != owner_id:
                    continue
                r = {k: v for k, v in rec.items() if k not in ("key_hash", "owner_id")}
                out.append(r)
            return out

    def revoke(self, kid, owner_id):
        with self.lock:
            rec = self.data["keys"].get(kid)
            if not rec or rec.get("owner_id", "local") != owner_id:
                return None
            rec["status"] = "revoked"
            self._save()
            return rec


KEYS = KeyStore(os.path.join(os.path.dirname(os.path.abspath(__file__)), "api_keys.json"),
                AUTH.legacy_owner_id)
SESSIONS = SessionStore(SESSION_TTL)


# ------------------------------------------------------------------- HTTP 服务 --

class Handler(BaseHTTPRequestHandler):
    server_version = "StructuredAI/2.0"
    protocol_version = "HTTP/1.1"

    # ---- 基础工具 ----
    def _cors(self):
        # Console credentials are same-origin only; calculation API stays public CORS.
        if self.path.startswith("/f/a/o/platform/"):
            self.send_header("Cache-Control", "no-store")
            self.send_header("Referrer-Policy", "no-referrer")
            return
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Access-Control-Allow-Headers", "Authorization, Content-Type")
        self.send_header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")

    def _send_json(self, status, obj, headers=()):
        data = json.dumps(obj, ensure_ascii=False).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        if status >= 400:
            # Auth failures may leave an unread request body. Do not reuse that connection.
            self.close_connection = True
            self.send_header("Connection", "close")
        for name, value in headers:
            self.send_header(name, value)
        self._cors()
        self.end_headers()
        self.wfile.write(data)

    def _read_body(self):
        try:
            length = int(self.headers.get("Content-Length") or 0)
        except ValueError:
            length = 0
        raw = self.rfile.read(length) if length > 0 else b""
        if not raw:
            return None, "Request body is empty."
        try:
            body = json.loads(raw.decode("utf-8"))
        except (json.JSONDecodeError, UnicodeDecodeError):
            return None, "Invalid JSON in request body."
        if not isinstance(body, dict):
            return None, "Request body must be a JSON object."
        return body, None

    # ---- 鉴权与静态页 ----
    ASSET_TYPES = {
        ".js": "text/javascript; charset=utf-8",
        ".css": "text/css; charset=utf-8",
        ".svg": "image/svg+xml",
        ".png": "image/png",
        ".ico": "image/x-icon",
        ".woff2": "font/woff2",
        ".map": "application/json",
    }

    def _serve_asset(self, name):
        """提供 web/assets/ 下的构建产物（白名单扩展名，防目录穿越）。"""
        base = os.path.realpath(os.path.join(WEB_DIR, "assets"))
        full = os.path.realpath(os.path.join(base, name))
        ext = os.path.splitext(full)[1]
        if not full.startswith(base + os.sep) or not os.path.isfile(full) or ext not in self.ASSET_TYPES:
            return self._send_json(404, {"error": {
                "message": f"Asset not found: {name}",
                "type": "invalid_request_error"}})
        try:
            with open(full, "rb") as f:
                data = f.read()
        except OSError:
            return self._send_json(404, {"error": {
                "message": f"Asset not found: {name}",
                "type": "invalid_request_error"}})
        self.send_response(200)
        self.send_header("Content-Type", self.ASSET_TYPES[ext])
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "public, max-age=31536000, immutable")
        self._cors()
        self.end_headers()
        self.wfile.write(data)

    def _auth_api_key(self):
        """校验平台签发的 Bearer API Key，成功返回 key 记录（并记住 id 供用量统计）。"""
        auth = self.headers.get("Authorization", "")
        raw = auth.split(" ", 1)[1].strip() if auth.startswith("Bearer ") else ""
        rec = KEYS.validate(raw)
        if not rec:
            self._send_json(*api_error(
                "Missing or invalid API key. Create one in the platform console "
                "(GET /platform) and pass it in the 'Authorization: Bearer sk-xxx' header.",
                err_type="authentication_error", code="invalid_api_key", status=401))
            return None
        self.api_key_id = rec["id"]
        return rec

    def _cookies(self):
        try:
            cookies = SimpleCookie()
            cookies.load(self.headers.get("Cookie", ""))
            return cookies
        except CookieError:
            return SimpleCookie()

    def _cookie_value(self, name):
        cookie = self._cookies().get(name)
        return cookie.value if cookie else ""

    def _cookie(self, name, value, ttl):
        cookies = SimpleCookie()
        cookies[name] = value
        cookie = cookies[name]
        cookie["path"] = "/" if name == SESSION_COOKIE else "/f/a/o/platform/auth/github"
        cookie["max-age"] = ttl
        cookie["httponly"] = True
        cookie["samesite"] = "Lax"
        if AUTH.public_url:
            secure = AUTH.public_url.startswith("https://")
        else:
            host = urlsplit("//" + self.headers.get("Host", "")).hostname
            secure = host not in ("127.0.0.1", "localhost", "::1")
        cookie["secure"] = secure
        return "Set-Cookie", cookie.OutputString()

    def _same_origin(self):
        origin = self.headers.get("Origin")
        if self.headers.get("Sec-Fetch-Site") == "cross-site":
            return False
        if not origin:
            return True
        if AUTH.public_url:
            return origin == AUTH.public_url
        try:
            url = urlsplit(origin)
        except ValueError:
            return False
        return (url.scheme in ("http", "https") and not url.path and
                url.netloc == self.headers.get("Host", ""))

    def _auth_session(self):
        """Cookie sessions require CSRF on writes; explicit header tokens support CLI use."""
        header_token = self.headers.get("X-Session-Token")
        token = header_token if header_token is not None else self._cookie_value(SESSION_COOKIE)
        session = SESSIONS.get(token)
        if not session:
            self._send_json(*api_error(
                "Invalid or expired session. Sign in again at /platform.",
                err_type="authentication_error", code="invalid_session", status=401))
            return False
        supplied_csrf = self.headers.get("X-CSRF-Token")
        if header_token is None and supplied_csrf is not None and not hmac.compare_digest(
                supplied_csrf.encode(), session["csrf_token"].encode()):
            self._send_json(*api_error("Session changed. Sign in again at /platform.",
                                      code="invalid_session", status=401))
            return False
        if self.command not in ("GET", "HEAD", "OPTIONS"):
            csrf = self.headers.get("X-CSRF-Token", "")
            if not self._same_origin() or (header_token is None and not hmac.compare_digest(
                    csrf.encode(), session["csrf_token"].encode())):
                self._send_json(*api_error("Invalid request origin or CSRF token.",
                                          code="invalid_csrf", status=403))
                return False
        self.session_token = token
        self.session = session
        self.owner_id = owner_id(session["user"])
        return True

    def _redirect(self, location, cookies=()):
        self.send_response(302)
        self.send_header("Location", location)
        self.send_header("Cache-Control", "no-store")
        self.send_header("Referrer-Policy", "no-referrer")
        for name, value in cookies:
            self.send_header(name, value)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def _github_callback(self):
        cleared = self._cookie(STATE_COOKIE, "", 0)
        try:
            if not AUTH.github_enabled:
                raise OAuthError("github_not_configured")
            query = parse_qs(urlsplit(self.path).query)
            states = query.get("state", [])
            if len(states) != 1:
                raise OAuthError("invalid_oauth_state")
            verifier = OAUTH.consume(states[0], self._cookie_value(STATE_COOKIE))
            if "error" in query:
                raise OAuthError("github_cancelled")
            codes = query.get("code", [])
            if len(codes) != 1 or len(codes[0]) > 1024:
                raise OAuthError("github_unavailable")
            user = OAUTH.exchange(codes[0], verifier)
            SESSIONS.revoke(self._cookie_value(SESSION_COOKIE))
            token = SESSIONS.create(user)
            return self._redirect("/platform", [cleared, self._cookie(SESSION_COOKIE, token, SESSION_TTL)])
        except OAuthError as exc:
            return self._redirect("/platform?login_error=" + str(exc), [cleared])

    def _serve_html(self, filename):
        path = os.path.join(WEB_DIR, filename)
        try:
            with open(path, encoding="utf-8") as f:
                data = f.read().encode("utf-8")
        except FileNotFoundError:
            return self._send_json(404, {"error": {
                "message": f"Page not found: web/{filename}",
                "type": "invalid_request_error"}})
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        self._cors()
        self.end_headers()
        self.wfile.write(data)

    def _record_usage(self, usage):
        kid = getattr(self, "api_key_id", None)
        if kid:
            KEYS.record_usage(kid, usage["prompt_tokens"], usage["completion_tokens"])

    # ---- 路由 ----
    def do_OPTIONS(self):  # noqa: N802
        self.send_response(204)
        self._cors()
        self.send_header("Content-Length", "0")
        self.end_headers()

    def do_GET(self):  # noqa: N802
        path = self.path.split("?", 1)[0]
        if path in ("/", "/index.html"):
            return self._serve_html("index.html")
        if path == "/platform":
            return self._serve_html("platform.html")
        if path == "/docs":
            return self._serve_html("docs.html")
        if path.startswith("/assets/"):
            return self._serve_asset(path[len("/assets/"):])
        if path == "/f/a/o/platform/auth":
            return self._send_json(200, {"github_enabled": AUTH.github_enabled})
        if path == "/f/a/o/platform/me":
            if not self._auth_session():
                return
            return self._send_json(200, {"user": self.session["user"],
                                         "csrf_token": self.session["csrf_token"]})
        if path == "/f/a/o/platform/auth/github":
            try:
                state, location = OAUTH.begin()
                return self._redirect(location, [self._cookie(STATE_COOKIE, state, STATE_TTL)])
            except OAuthError:
                return self._redirect("/platform?login_error=github_not_configured")
        if path == CALLBACK_PATH:
            return self._github_callback()
        if path == "/f/a/o/operator":
            if not self._auth_api_key():
                return
            data = []
            for code in sorted(OPERATOR_CODES):
                sym = OPERATOR_CODES[code]
                aliases = sorted(a for a, s2 in OP_ALIASES.items() if s2 == sym)
                data.append({"code": code, "symbol": sym,
                             "name": OPERATORS[sym][1], "aliases": aliases})
            return self._send_json(200, {"object": "operator_code_table", "data": data})
        if path == "/f/a/o/platform/keys":
            if not self._auth_session():
                return
            return self._send_json(200, {"object": "list", "data": KEYS.list(self.owner_id)})
        if path == "/f/a/o/platform/usage":
            if not self._auth_session():
                return
            keys = KEYS.list(self.owner_id)
            total = {"requests": 0, "prompt_tokens": 0,
                     "completion_tokens": 0, "total_tokens": 0}
            for k in keys:
                for field in total:
                    total[field] += k["usage"][field]
            return self._send_json(200, {"object": "usage_report",
                                         "total": total, "keys": keys})
        return self._send_json(404, {"error": {
            "message": f"Unknown request URL (GET {path}). Available: / /platform /docs "
                       "GET /f/a/o/operator, GET /f/a/o/platform/keys|usage",
            "type": "invalid_request_error"}})

    def do_POST(self):  # noqa: N802
        path = self.path.split("?", 1)[0]

        if path == "/f/a/o/platform/logout":
            if not self._auth_session():
                return
            SESSIONS.revoke(self.session_token)
            return self._send_json(200, {"ok": True}, [self._cookie(SESSION_COOKIE, "", 0)])

        # ---- 平台：创建 Key（需会话）----
        if path == "/f/a/o/platform/keys":
            if not self._auth_session():
                return
            body, err = self._read_body()
            if err:
                return self._send_json(*api_error(err))
            raw, rec = KEYS.create(body.get("name"), self.owner_id)
            return self._send_json(200, {
                "id": rec["id"], "name": rec["name"], "key": raw,
                "prefix": rec["prefix"], "created_at": rec["created_at"],
                "note": "Save this key now; it will not be shown again.",
            })

        # ---- 运算接口（需 API Key）----
        if path != "/f/a/o/calculate":
            return self._send_json(404, {"error": {
                "message": f"Unknown request URL (POST {path}). "
                           "Only POST /f/a/o/calculate is available.",
                "type": "invalid_request_error"}})
        if not self._auth_api_key():
            return

        body, err = self._read_body()
        if err:
            return self._send_json(*api_error(err))

        # 唯一输入形态：表达式字符串
        expr, perr = extract_expression(body)
        if perr:
            return self._send_json(*api_error(perr))
        try:
            result, steps, prefix_tokens, prefix_str = evaluate_full(expr)
        except ExprError as e:
            return self._send_json(*api_error(str(e), code=e.code))

        # 运算过程开关：开启 → SSE 流式输出过程；不开启 → 直接输出答案
        if body.get("process"):
            return self._stream_eval(expr, result, steps, prefix_str)
        usage = build_usage(expr, [])
        self._record_usage(usage)
        return self._send_json(200, {
            "id": "eval-" + uuid.uuid4().hex[:16],
            "object": "evaluation",
            "created": int(time.time()),
            "model": MODEL_ID,
            "expression": expr,
            "result": format_num(result),
            "usage": usage,
        })

    def do_DELETE(self):  # noqa: N802
        path = self.path.split("?", 1)[0]
        if path.startswith("/f/a/o/platform/keys/"):
            if not self._auth_session():
                return
            kid = path.rsplit("/", 1)[1]
            rec = KEYS.revoke(kid, self.owner_id)
            if not rec:
                return self._send_json(404, {"error": {
                    "message": f"Unknown key id: {kid}",
                    "type": "invalid_request_error"}})
            return self._send_json(200, {"id": rec["id"], "status": rec["status"]})
        return self._send_json(404, {"error": {
            "message": f"Unknown request URL (DELETE {path}).",
            "type": "invalid_request_error"}})

    # ---- 运算过程模式（process=true）：SSE 流式输出 prefix -> step -> final ----
    def _stream_eval(self, expr, result, steps, prefix_str):
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream; charset=utf-8")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "close")
        self._cors()
        self.end_headers()
        self.close_connection = True

        def emit(obj, delay=0.05):
            self.wfile.write(("data: " + json.dumps(obj, ensure_ascii=False) + "\n\n").encode("utf-8"))
            self.wfile.flush()
            time.sleep(delay)

        try:
            emit({"type": "prefix", "prefix": prefix_str})
            for st in steps:
                emit({"type": "step", "cost_tokens": count_tokens(st["expr"]), **st})
            usage = build_usage(expr, steps)
            self._record_usage(usage)
            emit({"type": "final", "expression": expr, "result": format_num(result),
                  "steps_count": len(steps), "usage": usage})
            self.wfile.write(b"data: [DONE]\n\n")
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass  # 客户端提前断开

    def log_request(self, code="-", size="-"):
        # OAuth codes and state belong in neither logs nor analytics.
        self.log_message('"%s %s %s" %s %s', self.command,
                         self.path.split("?", 1)[0], self.request_version, code, size)

    def log_message(self, fmt, *args):
        print(f"[{time.strftime('%H:%M:%S')}] {self.address_string()} {fmt % args}")


def main():
    server = ThreadingHTTPServer((HOST, PORT), Handler)
    print(f"结构化 AI API 已启动: http://{HOST}:{PORT}")
    print(f"  官网: http://{HOST}:{PORT}/  |  控制台: /platform  |  文档: /docs")
    print(f"  GitHub 登录: {'已启用（多用户）' if AUTH.github_enabled else '未配置'}")
    print(f"  API 鉴权: 控制台创建 API Key 后携带 Authorization: Bearer sk-xxx")
    print(f"  运算符编码: {CODE_TABLE_HINT}（先序 IR 中运算符使用编码）")
    print('  输入: {"expression": "3+5*2**(1+1)"}；加 "process": true 开启运算过程')
    print(f"  输出: 默认直接返回答案；process=true 走 SSE 事件流（prefix → step → final → [DONE]）")
    print(f"  计费: prompt=表达式 token 数；开启过程时 reasoning=各步 token 之和，答案=1；按 Key 累计")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\n服务已停止")


if __name__ == "__main__":
    main()
