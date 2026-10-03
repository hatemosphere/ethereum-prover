#!/usr/bin/env python3
"""Fake Ethereum JSON-RPC node that replays archived blocks, for running the prover
service offline.

Serves `eth_blockNumber`, `eth_chainId`, `eth_getBlockByNumber` and
`debug_executionWitness` from block directories `<dir>/<block>/` holding `block.json`
and `witness.json` (or `execution_witness.json`), as raw JSON-RPC responses or plain
results. The chain head walks through the archived block numbers in order, one step every
`--interval` seconds, starting at `--start` (default: the first archived block); with
`--interval 0` the head stays at the last archived block. Receipts are answered with null.

    scripts/fake_node.py --blocks-dir <dir> [--blocks-dir <dir> ...] --port 8546 --interval 15
"""
import argparse
import json
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


def load_result(path):
    value = json.loads(path.read_text())
    return value["result"] if isinstance(value, dict) and "jsonrpc" in value else value


class Archive:
    def __init__(self, dirs):
        self.blocks = {}
        for root in dirs:
            for entry in Path(root).iterdir():
                if entry.is_dir() and entry.name.isdigit() and (entry / "block.json").exists():
                    witness = next(
                        (entry / name for name in ("witness.json", "execution_witness.json")
                         if (entry / name).exists()),
                        None,
                    )
                    if witness is not None:
                        self.blocks[int(entry.name)] = (entry / "block.json", witness)
        if not self.blocks:
            sys.exit("no archived blocks found")
        self.numbers = sorted(self.blocks)

    def block(self, number, full):
        if number not in self.blocks:
            return None
        block = load_result(self.blocks[number][0])
        if not full:
            block = dict(block, transactions=[tx["hash"] for tx in block["transactions"]])
        return block

    def witness(self, number):
        return load_result(self.blocks[number][1]) if number in self.blocks else None


class Chain:
    """The head walks through the archived numbers, one step per interval."""

    def __init__(self, numbers, start, interval):
        self.numbers = numbers
        self.index = next((i for i, n in enumerate(numbers) if n >= start), 0) if start else 0
        self.interval = interval
        self.started = time.monotonic()
        if interval == 0:
            self.index = len(numbers) - 1

    def head(self):
        if self.interval == 0:
            return self.numbers[-1]
        steps = int((time.monotonic() - self.started) / self.interval)
        return self.numbers[min(self.index + steps, len(self.numbers) - 1)]


def make_handler(archive, chain, log):
    def block_number(param):
        if param in ("latest", "safe", "finalized", "pending"):
            return chain.head()
        return int(param, 16)

    def dispatch(request):
        method, params = request.get("method"), request.get("params") or []
        if method == "eth_blockNumber":
            result = hex(chain.head())
        elif method == "eth_chainId":
            result = "0x1"
        elif method == "eth_getBlockByNumber":
            number = block_number(params[0])
            result = archive.block(number, bool(params[1]) if len(params) > 1 else False)
            if number > chain.head():
                result = None
        elif method == "debug_executionWitness":
            number = block_number(params[0])
            result = archive.witness(number) if number <= chain.head() else None
            if result is None:
                return {"jsonrpc": "2.0", "id": request.get("id"),
                        "error": {"code": -32000, "message": f"no witness for block {number}"}}
        elif method == "eth_getTransactionReceipt":
            result = None
        else:
            return {"jsonrpc": "2.0", "id": request.get("id"),
                    "error": {"code": -32601, "message": f"method {method} not supported"}}
        log.write(f"{time.strftime('%H:%M:%S')} {method} {params[:1]}\n")
        log.flush()
        return {"jsonrpc": "2.0", "id": request.get("id"), "result": result}

    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            response = [dispatch(r) for r in body] if isinstance(body, list) else dispatch(body)
            payload = json.dumps(response).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def log_message(self, *args):
            pass

    return Handler


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--blocks-dir", action="append", required=True)
    parser.add_argument("--port", type=int, default=8546)
    parser.add_argument("--start", type=int, default=0, help="first head (archived block number)")
    parser.add_argument("--interval", type=float, default=12.0, help="seconds per head step; 0 = fixed at the last block")
    parser.add_argument("--log", default="-", help="request log file ('-' = stderr)")
    args = parser.parse_args()

    archive = Archive(args.blocks_dir)
    chain = Chain(archive.numbers, args.start, args.interval)
    log = sys.stderr if args.log == "-" else open(args.log, "a")
    print(f"serving {len(archive.numbers)} blocks {archive.numbers[0]}..{archive.numbers[-1]} "
          f"on 127.0.0.1:{args.port}", file=sys.stderr, flush=True)
    server = ThreadingHTTPServer(("127.0.0.1", args.port), make_handler(archive, chain, log))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        threading.Event().wait()
    except KeyboardInterrupt:
        server.shutdown()


if __name__ == "__main__":
    main()
