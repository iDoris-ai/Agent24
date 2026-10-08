import json, sys, subprocess
a = json.loads(subprocess.check_output([sys.executable, '-I', 'probe.py', sys.argv[1]]))[0]
for f in sys.argv[2:]:
    b = json.loads(subprocess.check_output([sys.executable, '-I', 'probe.py', f]))[0]
    print('==', f)
    for k in a:
        if k in ('file',): continue
        if a[k] != b[k]:
            print(f'  {k}:\n    before {json.dumps(a[k], ensure_ascii=False)}\n    after  {json.dumps(b[k], ensure_ascii=False)}')
