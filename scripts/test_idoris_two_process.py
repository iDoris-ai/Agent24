#!/usr/bin/env python3
"""IDORIS-06: real Agent24 router -> real iDoris -> loopback fixture.

Supply iDoris's own test_agent24_idoris_v031.py as --wire-harness; it remains
the owner of key initialization, process cleanup, recording tap and canaries.
This consumer test exercises TaskProfile daily/deep, not model override.
"""
import argparse
import hashlib
import http.server
import importlib.util
import json
import subprocess
import tempfile
import threading
from pathlib import Path


class BudgetFixture:
    def __init__(self, wire):
        self.seen = []
        self.gets = []
        fixture = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def reply(self, data):
                raw = json.dumps(data).encode()
                self.send_response(200)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)

            def do_GET(self):
                fixture.gets.append(self.path)
                if self.path == '/api/status':
                    self.reply({'loaded_models': [], 'model_memory_used': 0,
                                'model_memory_max': 16, 'pressure': 'ok'})
                elif self.path == '/v1/models/status':
                    self.reply({'models': [{'id': name, 'loaded': True, 'pinned': False,
                                            'estimated_size': 1073741824}
                                           for name in set(wire.ROLE_MODELS.values())]})
                else:
                    self.reply({'object': 'list', 'data': [
                        {'id': name, 'object': 'model'} for name in set(wire.ROLE_MODELS.values())]})

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers.get('Content-Length', 0))) or b'{}')
                fixture.seen.append({'path': self.path, 'body': body,
                                     'virtual_key_forwarded': 'idk_' in self.headers.get('Authorization', '')})
                self.reply({'id': 'wire-fixture', 'model': body.get('model'),
                            'choices': [{'index': 0, 'finish_reason': 'stop', 'message': {
                                'role': 'assistant', 'content': wire.MARKER}}],
                            'usage': {'prompt_tokens': 1, 'completion_tokens': 1, 'total_tokens': 2}})

        self.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.port = self.server.server_port
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(5)


def allocation(binary, reserved):
    result = subprocess.run([binary, 'hw', 'idoris-env', '--core-reserved-bytes', str(reserved)],
                            capture_output=True, text=True, timeout=30, check=True)
    name, value = result.stdout.strip().split('=')
    assert name == 'IDORIS_ALLOCATED_BUDGET_BYTES' and value.isdecimal(), result.stdout
    return value


def role_cases(wire, args):
    settings = wire.mk_args(agent24_bin=args.agent24_probe,
                            agent24_call_cmd=json.dumps([args.agent24_probe, '{role}', '{prompt}']))
    template = json.loads(settings.agent24_call_cmd)
    canaries, fixture, gateway, tap = wire.Canaries(), wire.Fixture(), None, None
    try:
        assert not canaries.unavailable, 'default backend canaries unavailable; do not stop existing services'
        with tempfile.TemporaryDirectory(prefix='idoris-role-wire-') as temporary:
            root = Path(temporary)
            home = root / 'home'
            home.mkdir()
            gateway = wire.Gateway(args.idoris_bin, args.idoris_config_dir, fixture.port, root)
            gateway.env['HOME'] = str(home)
            full = gateway.issue_key('a24-wire-full', wire.ALL_ROLES)
            daily = gateway.issue_key('a24-wire-daily', ['daily'])
            gateway.start()
            tap = wire.Tap(gateway.port, wire.sha(full))
            context = dict(args=settings, tap=tap, fixture=fixture, canaries=canaries,
                           gw=gateway, root=root, home=home, cwd=root,
                           agent_bin=args.agent24_probe, template=template,
                           tap_url='http://127.0.0.1:%d' % tap.port, full=full, daily_only=daily)
            plan = [('happy_role_daily', 'full', 'idoris/daily', 'none', 'ok'),
                    ('happy_role_deep', 'full', 'idoris/deep', 'none', 'ok'),
                    ('no_key', None, 'idoris/daily', 'none', 'reject'),
                    ('wrong_key', 'idk_' + 'x' * 40, 'idoris/daily', 'none', 'reject'),
                    ('role_not_permitted', 'daily_only', 'idoris/deep', 'none', 'reject'),
                    ('locality_remote', 'full', 'idoris/daily', 'locality_remote', 'reject'),
                    ('locality_lan', 'full', 'idoris/daily', 'locality_lan', 'reject'),
                    ('locality_missing', 'full', 'idoris/daily', 'locality_missing', 'reject'),
                    ('gateway_down', 'full', 'idoris/daily', 'kill', 'reject')]
            results = [wire.scenario(context, *case) for case in plan]
            scrub = wire.Scrubber()
            scrub.add_secret(full)
            scrub.add_secret(daily)
            return json.loads(scrub(json.dumps(results)))
    finally:
        if tap:
            tap.close()
        if gateway:
            gateway.stop()
        canaries.close()
        fixture.close()


def budget_case(wire, args, budget, expected):
    fixture = BudgetFixture(wire)
    gateway = None
    with tempfile.TemporaryDirectory(prefix='idoris-budget-wire-') as temporary:
        root = Path(temporary)
        home = root / 'home'
        home.mkdir()
        try:
            gateway = wire.Gateway(args.idoris_bin, args.idoris_config_dir, fixture.port, root)
            card = root / 'components' / 'local.yaml'
            card.write_text(card.read_text().replace('id: fixture-local', 'id: omlx')
                            .replace('mode: resident', 'mode: on_demand')
                            .replace('pinned: true', 'idle_ttl_s: 300')
                            .replace('admission: coexist', 'admission: requires_eviction'))
            gateway.env['HOME'] = str(home)
            gateway.env['IDORIS_ALLOCATED_BUDGET_BYTES'] = budget
            key = gateway.issue_key('a24-budget-wire', wire.ALL_ROLES)
            gateway.start()
            environment = wire.child_env(home, 'http://127.0.0.1:%d' % gateway.port, key, wire.mk_args())
            result = wire.run_consumer([args.agent24_probe, 'simple', 'budget wire'], environment, root, 30)
            posts = fixture.seen[:]
            if expected:
                assert result['exit_code'] == 0 and wire.MARKER in result['stdout'], result
                chats = [p for p in posts if p['path'] == '/v1/chat/completions']
                assert chats and any(p['path'].endswith('/load') for p in posts), posts
                body = chats[-1]['body']
                assert body['model'] == wire.ROLE_MODELS['daily'], body
                assert body['max_tokens'] == 512 and body['tools'][0]['function']['name'] == 'probe_tool', body
                assert body['response_format']['type'] == 'json_schema', body
                assert body['chat_template_kwargs']['enable_thinking'] is False, body
            else:
                assert result['exit_code'] not in (0, None) and not result['timed_out'], result
                assert not posts and wire.MARKER not in result['stdout'], posts
            assert not any(p['virtual_key_forwarded'] for p in posts), posts
            return {'allocated_bytes': budget, 'expected_success': expected, 'passed': True,
                    'gets': len(fixture.gets), 'loads': sum(p['path'].endswith('/load') for p in posts),
                    'chats': sum(p['path'] == '/v1/chat/completions' for p in posts)}
        finally:
            if gateway:
                gateway.stop()
            fixture.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for option in ('wire-harness', 'idoris-bin', 'idoris-config-dir', 'agent24-probe', 'agent24-bin', 'report-json'):
        parser.add_argument('--' + option, required=True)
    args = parser.parse_args()
    spec = importlib.util.spec_from_file_location('idoris_wire', args.wire_harness)
    wire = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(wire)
    # Positive control must use the actual hardware sender; zero is a real
    # allocation from the same command, not an absent environment variable.
    available = allocation(args.agent24_bin, 0)
    zero = allocation(args.agent24_bin, 18446744073709551615)
    assert zero == '0'
    insufficient = allocation(args.agent24_bin, max(0, int(available) - 500000000))
    assert 0 < int(insufficient) < 1073741824 and int(available) > 2000000000
    report = {'schema': 'agent24-idoris-two-process/1', 'passed': False,
              'coverage_gaps': ['Loopback fixture, no real model or hardware release acceptance.',
                                'TaskProfile local_only only; concrete model override/streaming excluded.',
                                'This script does not prove cancellation, timeout or missing backend gates.'],
              'input_sha256': {name: hashlib.sha256(Path(value).read_bytes()).hexdigest()
                               for name, value in vars(args).items()
                               if name in ('wire_harness', 'idoris_bin', 'agent24_probe', 'agent24_bin')},
              'budget_cases': [], 'role_cases': []}
    try:
        report['role_cases'] = role_cases(wire, args)
        assert all(case['pass'] for case in report['role_cases']), report['role_cases']
        for budget, expected in [(zero, False), (insufficient, False), (available, True)]:
            report['budget_cases'].append(budget_case(wire, args, budget, expected))
        report['passed'] = True
    finally:
        Path(args.report_json).write_text(json.dumps(report, indent=2) + '\n')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
