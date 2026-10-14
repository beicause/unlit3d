#!/usr/bin/env python3
'''Drive a unlit3d MCP host over stdio.

The host must be started with its --mcp switch; this client spawns it, does the
MCP handshake, runs the operations given as a JSON array on the command line (or
on stdin), prints each result, and terminates the host.

An operation is one of:
  {"call": "<tool>", "args": {...}, "max": 4000}   call a tool
  {"listTools": true}                              list the tools
  {"sleep": 6}                                     wait before the next one

Which host to drive, and with which arguments, differs per host, so the binary
is always named explicitly.

Usage:
  mcp_drive.py --bin ./target/debug/unlit3dx-examples '[{"call": "list_worlds"}, {"sleep": 6},
                 {"call": "save_screenshot",
                  "args": {"world": {"Handle": 5}, "directory": "/tmp"}}]'
  mcp_drive.py --bin ./target/debug/unlit3d-examples \\
               --args '--mcp --scene spin_cube' '[{"call": "list_worlds"}]'
'''
import argparse
import json
import os
import subprocess
import sys
import time

DEFAULT_ARGS = '--mcp --stepped'


class Client:
    def __init__(self, binary, arguments, timeout):
        self.timeout = timeout
        self.proc = subprocess.Popen(
            [binary] + arguments.split(),
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            bufsize=0,
        )
        self.next_id = 0
        self.request('initialize', {
            'protocolVersion': '2024-11-05',
            'capabilities': {},
            'clientInfo': {'name': 'mcp-drive', 'version': '1'},
        })
        self.notify('notifications/initialized')

    def send(self, message):
        self.proc.stdin.write((json.dumps(message) + chr(10)).encode())
        self.proc.stdin.flush()

    def request(self, method, params=None):
        self.next_id += 1
        message = {'jsonrpc': '2.0', 'id': self.next_id, 'method': method}
        if params is not None:
            message['params'] = params
        self.send(message)
        return self.await_reply(self.next_id)

    def notify(self, method, params=None):
        message = {'jsonrpc': '2.0', 'method': method}
        if params is not None:
            message['params'] = params
        self.send(message)

    def await_reply(self, request_id):
        deadline = time.time() + self.timeout
        while time.time() < deadline:
            line = self.proc.stdout.readline()
            if not line:
                return None
            line = line.strip()
            if not line:
                continue
            try:
                message = json.loads(line)
            except ValueError:
                continue
            if message.get('id') == request_id:
                return message
        return None

    def call(self, name, arguments, max_length):
        reply = self.request('tools/call', {'name': name, 'arguments': arguments})
        print('== %s %s' % (name, json.dumps(arguments, ensure_ascii=False)))
        if reply is None:
            print('   no reply within %gs' % self.timeout)
            return
        result = reply.get('result', reply.get('error'))
        text = json.dumps(result, ensure_ascii=False)
        if len(text) <= max_length:
            print(text)
        else:
            print('%s ... (%d chars in all)' % (text[:max_length], len(text)))

    def stop(self):
        self.proc.terminate()


def main():
    parser = argparse.ArgumentParser(description='Drive a unlit3d MCP host over stdio.')
    parser.add_argument('--bin', required=True,
                        help='the host binary to spawn, with --mcp among its arguments')
    parser.add_argument('--args', default=os.environ.get('EXAMPLE_ARGS', DEFAULT_ARGS),
                        help='the host arguments (default %s)' % DEFAULT_ARGS)
    parser.add_argument('--timeout', type=float, default=30.0,
                        help='seconds to wait for one reply')
    parser.add_argument('operations', nargs='?',
                        help='a JSON array of operations; read from stdin when omitted')
    parsed = parser.parse_args()

    text = parsed.operations if parsed.operations is not None else sys.stdin.read()
    operations = json.loads(text)

    client = Client(parsed.bin, parsed.args, parsed.timeout)
    try:
        for operation in operations:
            if 'sleep' in operation:
                time.sleep(operation['sleep'])
                print('-- slept %gs' % operation['sleep'])
            elif operation.get('listTools'):
                reply = client.request('tools/list')
                tools = (reply or {}).get('result', {}).get('tools', [])
                for tool in tools:
                    print('%s -- %s' % (tool['name'], tool.get('description', '')))
            else:
                client.call(operation['call'], operation.get('args', {}),
                            operation.get('max', 4000))
    finally:
        client.stop()


if __name__ == '__main__':
    main()
