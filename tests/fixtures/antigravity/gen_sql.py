import sys
def v(n):
    o=bytearray()
    while True:
        b=n&0x7f; n>>=7
        if n: o.append(b|0x80)
        else: o.append(b); return bytes(o)
def f_var(no,n): return v(no<<3)+v(n)
def f_bytes(no,b):
    if isinstance(b,str): b=b.encode()
    return v(no<<3|2)+v(len(b))+b
def ts(sec,nanos=0): return f_var(1,sec)+(f_var(2,nanos) if nanos else b'')
T0=1795168800
def usage(enum,inp,think,resp,cw,cr,rid):
    u=f_var(1,enum)+f_var(2,inp)+f_var(3,think+resp)
    if cw: u+=f_var(4,cw)
    if cr: u+=f_var(5,cr)
    u+=f_var(9,think)+f_var(10,resp)
    if rid: u+=f_bytes(11,rid)
    return u
def gen(label,rmodel,u,t=None):
    chat=b''
    if rmodel: chat+=f_bytes(19,rmodel)
    if label: chat+=f_bytes(21,label)
    chat+=f_bytes(4,u)
    d=f_bytes(1,chat)
    if t is not None: d+=f_bytes(9,f_var(1,7)+f_bytes(4,ts(*t)))
    return d
def step(t,rid=None,gi=None):
    m=f_bytes(1,ts(*t))
    if rid: m+=f_bytes(9,f_bytes(11,rid))
    if gi is not None: m+=f_bytes(20,f_var(3,gi))
    return m
def traj(uri,t): return f_bytes(1,f_bytes(1,uri))+f_bytes(2,ts(*t))
H=lambda b:"X'"+b.hex()+"'"
SCHEMA="""CREATE TABLE gen_metadata (idx integer, data blob, size integer);
CREATE TABLE trajectory_metadata_blob (id text, data blob);
CREATE TABLE steps (idx integer, step_type integer, metadata blob);
"""
def write(path, cid, uri, t0, gens, steps):
    out=[SCHEMA]
    out.append(f"INSERT INTO trajectory_metadata_blob VALUES ('{cid}', {H(traj(uri,(t0,)))});")
    for i,(comment,d) in enumerate(gens):
        out.append(f"-- {comment}\nINSERT INTO gen_metadata VALUES ({i}, {H(d)}, {len(d)});")
    for i,(st,m) in enumerate(steps):
        out.append(f"INSERT INTO steps VALUES ({i}, {st}, {H(m) if m else 'NULL'});")
    open(path,'w').write("\n".join(out)+"\n")
out=sys.argv[1]
G36="Gemini 3.6 Flash (High)"
write(f"{out}/app.sql","5c1e9a7b-3d2f-4e6a-8b0c-1f2e3d4c5b6a","file:///u/user/pale%20fjord-8a",T0,[
 ("Gemini 3.6 Flash: own timestamp, thinking + text output", gen(G36,"gemini-3-flash-a",usage(1132,1200,300,150,0,0,"resp_7Qm2xA1"),(T0+10,500000000))),
 ("Gemini 3.6 Flash: cache read", gen(G36,"gemini-3-flash-a",usage(1132,300,50,150,0,1100,"resp_7Qm2xB2"),(T0+40,))),
 ("Claude Sonnet 4.5: no own timestamp, matched to a step by response id; cache write", gen("Claude Sonnet 4.5 (Thinking)",None,usage(1026,2000,200,400,1500,0,"resp_7Qm2xC3"))),
 ("all-zero usage (failed call): skipped", gen(G36,"gemini-3-flash-a",usage(1132,0,0,0,0,0,None),(T0+130,))),
],[
 (14,None),
 (15,step((T0+10,),"resp_7Qm2xA1",0)),
 (15,step((T0+40,),"resp_7Qm2xB2",1)),
 (15,step((T0+95,),"resp_7Qm2xC3",2)),
])
write(f"{out}/cli.sql","8e2d4c6b-1a3f-4b5e-9c7d-0e1f2a3b4c5d","file:///u/user/keen-lagoon-45",T0+86400,[
 ("no label, only the response model; timestamp from the step with generation index 0", gen(None,"gemini-3.1-pro-preview",usage(1031,4000,700,300,0,2500,"resp_9Kp4yD4"))),
 ("no label and no step: falls back to the conversation's created-at", gen(None,"gemini-3.1-pro-preview",usage(1031,250,0,80,0,4000,"resp_9Kp4yE5"))),
],[
 (15,step((T0+86400+20,),None,0)),
])
