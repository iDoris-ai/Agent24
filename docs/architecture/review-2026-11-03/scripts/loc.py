import os,re,sys
# 非测试代码行：非空、非纯注释行。
# Rust：只扫 src/ 与 build.rs；跳过 tests/ 目录、文件名 tests.rs / *_tests.rs / *_test.rs；
#       顶格 `#[cfg(test)]` 之后紧跟 `mod` 的块，从该行起到文件尾全部视为测试（仓库惯例：测试模块放文件尾）。
# TS：扫 src/（无 src 则扫包根），跳过 node_modules/dist/build/out/test/tests/__tests__/e2e，跳过 *.test.* / *.spec.*。
SKIPD={'node_modules','dist','build','out','tests','test','__tests__','e2e','target','.vite','coverage','fixtures'}
def rs_count(path):
    L=open(path,encoding='utf-8',errors='ignore').read().split('\n')
    n=0
    for i,l in enumerate(L):
        if l.startswith('#[cfg(test)]'):
            nxt=next((x for x in L[i+1:] if x.strip()),'')
            if re.match(r'(pub(\(crate\))? )?mod ',nxt): break
        s=l.strip()
        if not s or s.startswith('//'): continue
        n+=1
    return n
def ts_count(path):
    n=0;inb=False
    for l in open(path,encoding='utf-8',errors='ignore'):
        s=l.strip()
        if inb:
            if '*/' in s: inb=False
            continue
        if not s or s.startswith('//'): continue
        if s.startswith('/*'):
            if '*/' not in s: inb=True
            continue
        n+=1
    return n
def walk(root,lang):
    tot=0;files={}
    for d,ds,fs in os.walk(root):
        ds[:]=[x for x in ds if x not in SKIPD]
        for f in fs:
            p=os.path.join(d,f)
            if lang=='rs':
                if not f.endswith('.rs') or f=='tests.rs' or f.endswith('_tests.rs') or f.endswith('_test.rs'): continue
                c=rs_count(p)
            else:
                if not re.search(r'\.(ts|tsx|mts|js|mjs|cjs)$',f) or re.search(r'\.(test|spec)\.',f) or f.endswith('.d.ts'): continue
                c=ts_count(p)
            files[p]=c;tot+=c
    return tot,files
if __name__=='__main__':
    mode=sys.argv[1];root=sys.argv[2]
    tot,files=walk(root,mode)
    if len(sys.argv)>3:
        for p,c in sorted(files.items(),key=lambda x:-x[1])[:int(sys.argv[3])]: print(c,p)
    print(tot)
