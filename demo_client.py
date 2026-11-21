#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
演示客户端：调用 Lug 运算 API。
用法：
  1. 在控制台 http://127.0.0.1:8000/platform 登录并创建 API Key（明文只显示一次）
  2. export LUG_API_KEY=sk-xxxx   或   python3 demo_client.py sk-xxxx
  3. python3 demo_client.py
先启动服务端：python3 structured_ai_api.py
"""
import json
import os
import sys
import time
import urllib.request

BASE = "http://127.0.0.1:8000"
API_KEY = os.environ.get("LUG_API_KEY") or (sys.argv[1] if len(sys.argv) > 1 else None)

if not API_KEY:
    sys.exit("缺少 API Key：请先在控制台 /platform 创建，然后 "
             "export LUG_API_KEY=sk-xxxx 或 python3 demo_client.py sk-xxxx")

HEADERS = {"Authorization": f"Bearer {API_KEY}", "Content-Type": "application/json"}


def demo_operator():
    print("=" * 60)
    print("1) GET /f/a/o/operator —— 运算符编码表")
    req = urllib.request.Request(BASE + "/f/a/o/operator", headers=HEADERS)
    table = json.loads(urllib.request.urlopen(req).read())
    for op in table["data"]:
        print(f"  编码 {op['code']} -> {op['symbol']:<3} ({op['name']})")


def demo_calculate_non_stream():
    print("=" * 60)
    print("2) POST /f/a/o/calculate —— 默认：直接输出答案")
    payload = {"expression": "3+5*2**(1+1)"}
    req = urllib.request.Request(BASE + "/f/a/o/calculate",
                                 data=json.dumps(payload).encode(),
                                 headers=HEADERS, method="POST")
    t0 = time.time()
    resp = json.loads(urllib.request.urlopen(req, timeout=30).read())
    print(f"耗时 {(time.time()-t0)*1000:.1f}ms，响应字段：")
    print(f"  expression : {resp['expression']}")
    print(f"  result     : {resp['result']}")
    print(f"  usage      : {json.dumps(resp['usage'], ensure_ascii=False)}（无过程，reasoning=0）")


def demo_calculate_stream():
    print("=" * 60)
    print("3) POST /f/a/o/calculate process=true —— 开启运算过程：流式输出")
    payload = {"expression": "(3+5)*2**2/4", "process": True}
    req = urllib.request.Request(BASE + "/f/a/o/calculate",
                                 data=json.dumps(payload).encode(),
                                 headers=HEADERS, method="POST")
    t0 = time.time()
    resp = urllib.request.urlopen(req, timeout=30)
    for line in resp:
        line = line.decode("utf-8").strip()
        if not line.startswith("data: "):
            continue
        data = line[6:]
        if data == "[DONE]":
            print(f"  +{(time.time()-t0)*1000:5.0f}ms [DONE]")
            break
        obj = json.loads(data)
        tag = obj.get("type")
        if tag == "prefix":
            print(f"  +{(time.time()-t0)*1000:5.0f}ms [prefix] {obj['prefix']}")
        elif tag == "step":
            print(f"  +{(time.time()-t0)*1000:5.0f}ms [step {obj['step']}] "
                  f"{obj['expr']} = {obj['result']} (cost_tokens={obj['cost_tokens']})")
        elif tag == "final":
            print(f"  +{(time.time()-t0)*1000:5.0f}ms [final] {obj['expression']} = {obj['result']}")


def demo_errors():
    print("=" * 60)
    print("4) 错误处理 —— 除零 / 非表达式输入 / 无效 Key")
    for payload, headers, path in [
            ({"expression": "1/0"}, HEADERS, "/f/a/o/calculate"),
            ({"messages": []}, HEADERS, "/f/a/o/calculate"),
            ({"expression": "1+1"},
             {"Authorization": "Bearer sk-invalid", "Content-Type": "application/json"},
             "/f/a/o/calculate")]:
        try:
            req = urllib.request.Request(BASE + path, data=json.dumps(payload).encode(),
                                         headers=headers, method="POST")
            urllib.request.urlopen(req, timeout=30)
        except urllib.error.HTTPError as e:
            body = json.loads(e.read())
            print(f"  HTTP {e.code} [{body['error'].get('code')}]: {body['error']['message'][:70]}")


if __name__ == "__main__":
    demo_operator()
    demo_calculate_non_stream()
    demo_calculate_stream()
    demo_errors()
