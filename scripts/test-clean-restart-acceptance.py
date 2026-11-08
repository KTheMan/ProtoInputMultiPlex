"""Bounded same-original-process restart gate. SOURCE DRAFT, no execution until ROOT READY."""
import argparse
import copy
import ctypes
from ctypes import wintypes
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time


def module(name,filename):
    spec=importlib.util.spec_from_file_location(name,Path(__file__).with_name(filename));v=importlib.util.module_from_spec(spec);spec.loader.exec_module(v);return v


def digest(p):return hashlib.sha256(Path(p).read_bytes()).hexdigest()


def freeze(a):
    assert a.ready,"Await final independent source+producer+ROOT READY"
    assert digest(a.producer)==a.producer_sha256.lower();p=json.loads(a.producer.read_text(encoding="utf-8"))
    assert p["result"]=="PASS"and p["source_before_sha256"]==p["source_after_sha256"]
    assert len(p["checks"])==5 and all(c["exit_code"]==0 for c in p["checks"])
    assert p["native_build"]["exit_code"]==0 and p["doctor"]["exit_code"]==0,"Fresh native botharch+doctor required; no oldruntime reuse"
    root=Path(__file__).resolve().parent.parent
    assert all(digest(root/n)==h for n,h in p["source_after_sha256"].items())
    assert digest(a.executable)==p["build_sha256"]["x64"]and digest(a.x86_executable)==p["build_sha256"]["i686"]
    assert all(digest(a.runtime/n)==h for n,h in p["runtime_sha256"].items())
    assert a.phase!='native','Accepted same-PID positive subgroup is frozen; use native-tail only'
    if a.phase in('native-tail','gui'):
        assert a.from_run and a.from_run.resolve()==(root/'runtime/acceptance-clean-restart-native-20261015-101049').resolve()
        assert digest(a.from_run/'evidence.json')=='5a5b42efade63268bd80f6b94d1904a7e354d0210616980b754eb2937848c962'
        assert digest(a.from_run/'independent-prefix-review.json')=='026cc74bc6d7a2c043cf7105bd032c3f496a4940ac2274d9d673b95da17f0065'
        prior=json.loads((a.from_run/'evidence.json').read_text(encoding='utf-8'))
        assert prior['producer_sha256']==a.producer_sha256.lower()
    return p


def native(a,p):
    helper=module("restart_mcp","test-mcp.py");geometry=module("restart_geometry","owned-gui-geometry.py");pump=module("restart_pump","test-output-shutdown-timeout.py")
    geometry.user32.PostMessageW.argtypes=[wintypes.HWND,wintypes.UINT,wintypes.WPARAM,wintypes.LPARAM]
    run=a.runtime.resolve()/time.strftime("acceptance-clean-restart-native-%Y%m%d-%H%M%S");run.mkdir()
    print('RUN: '+str(run),flush=True)
    images={64:run/"control64.exe",32:run/"probe32.exe"}
    for source,bits in((a.executable,64),(a.x86_executable,32)):shutil.copy2(source,images[bits])
    e={"result":"NOT ACCEPTED","producer":str(a.producer),"producer_sha256":digest(a.producer),"build_sha256":{str(v):digest(v)for v in images.values()},"calls":[],"groups":{},"mutations":0,"routing_input_calls":0}
    e["phase"]=a.phase
    if a.from_run:e["retained_prefix"]={"path":str(a.from_run),"raw_sha256":digest(a.from_run/"evidence.json"),"review_sha256":digest(a.from_run/"independent-prefix-review.json")}
    clients=[];targets=[];logs=[];layers=set();workers=[];worker_handles={}
    kernel=ctypes.WinDLL('kernel32',use_last_error=True)
    kernel.OpenProcess.argtypes=[wintypes.DWORD,wintypes.BOOL,wintypes.DWORD];kernel.OpenProcess.restype=wintypes.HANDLE
    kernel.WaitForSingleObject.argtypes=[wintypes.HANDLE,wintypes.DWORD];kernel.WaitForSingleObject.restype=wintypes.DWORD
    kernel.GetExitCodeProcess.argtypes=[wintypes.HANDLE,ctypes.POINTER(wintypes.DWORD)]
    kernel.CloseHandle.argtypes=[wintypes.HANDLE]
    def save():(run/"evidence.json").write_text(json.dumps(e,indent=2,ensure_ascii=False)+'\n',encoding="utf-8")
    def report(t):return helper.eventually(lambda:json.loads(t["report"].read_text(encoding="utf-8")),lambda v:True,3)
    def connect(name):
        c=helper.Client(images[64],a.runtime.resolve(),run/(name+".stderr.log"));clients.append(c);return c
    def call(c,name,args=None,error=False):
        mutating=name not in("routing_state","routing_windows")
        if mutating:e["mutations"]+=1
        if name=="routing_input":e["routing_input_calls"]+=1
        assert e["mutations"]<=30 and e["routing_input_calls"]<=20
        r={"controller":c.process.pid,"tool":name,"arguments":args or {},"expected_error":error,"mutating":mutating,"started_ns":time.monotonic_ns()};e["calls"].append(r);save()
        try:r["result"]=c.tool(name,args,error=error)
        except(OSError,ValueError)as ex:raise RuntimeError("Imperative MCP request cannot be retried")from ex
        r["finished_ns"]=time.monotonic_ns()
        # Discard unrelated inventory before recording; only owned target metadata matters.
        if name=="routing_windows":r["result"]["windows"]=[w for w in r["result"]["windows"]if w["pid"]in{t["process"].pid for t in targets}]
        save();return r["result"]
    def state(c):return call(c,"routing_state")
    def inp(c,action,**kw):return call(c,"routing_input",{"action":action,**kw})
    def key(c,user,k,down):return inp(c,"key",user=user,key=k,pressed=down)
    def spawn(name,bits=64,cursor=False,is_pump=False):
        path=run/(name+".json")
        if is_pump:
            log=(run/(name+".stderr.log")).open("w",encoding="utf-8");logs.append(log)
            proc=subprocess.Popen([sys.executable,str(Path(__file__).with_name("test-output-shutdown-timeout.py").resolve()),"--fixture","--report",str(path)],stderr=log,creationflags=subprocess.CREATE_NO_WINDOW)
        else:proc=subprocess.Popen([str(images[bits]),"probe","--automation",*( ["--cursor-fixture"]if cursor else[]),"--report",str(path)],creationflags=subprocess.CREATE_NO_WINDOW)
        t={"name":name,"process":proc,"report":path,"pump":is_pump,"bits":bits};targets.append(t)
        r=helper.eventually(lambda:json.loads(path.read_text(encoding="utf-8")),lambda v:v["frames"]>2,12)
        t["root"]=int(r["hwnd"])if is_pump else int(helper.eventually(lambda:geometry.owned_window(proc.pid),bool,10))
        assert geometry.user32.SetWindowPos(t["root"],wintypes.HWND(-1),30+460*((len(targets)-1)%2),40,420,360,0x10)
        t["child"]=t["root"]if is_pump else r["capture_hwnd"]
        owner=wintypes.DWORD();assert geometry.user32.GetWindowThreadProcessId(t["root"],ctypes.byref(owner))and owner.value==proc.pid
        return t
    def close_target(t):
        proc=t["process"]
        if proc.poll()is None:
            assert geometry.user32.PostMessageW(t["root"],0x10,0,0);proc.wait(timeout=8)
        assert proc.returncode==0
    def inventory(c,ts):
        rows=call(c,"routing_windows")["windows"];result={}
        for t in ts:
            row=next(w for w in rows if w["pid"]==t["process"].pid)
            v=row["eligibility"];assert v["pid"]==t["process"].pid and v["creation"] and v["reason"]
            result[str(t["process"].pid)]=v
        return result
    def eligible(c,ts,allowed):
        return helper.eventually(lambda:inventory(c,ts),lambda v:all(x["state"]in allowed for x in v.values()),10)
    def signature(ts):
        out=[]
        for t in ts:
            r=report(t)
            if t["pump"]:
                discrete=[0x100,0x101,0x102,0x104,0x105,0x200,0x201,0x202,0x204,0x205,0x207,0x208,0x20a,0x20b,0x20c,0x20e]
                out.append({"keys":r["key_events"],"messages":{str(k):r["message_counts"].get(str(k),0)for k in discrete}})
            else:out.append(r["native_events"])
        return out
    def quiet(ts):
        clocks=[report(t)["frames"]for t in ts]
        helper.eventually(lambda:[report(t)for t in ts],lambda rs:all(r["frames"]>=clocks[i]+3 for i,r in enumerate(rs)),5)
    def focus(c,user,t,root=False):
        hwnd=t["root"]if root else t["child"]
        rect=geometry.Rect();assert geometry.user32.GetWindowRect(hwnd,ctypes.byref(rect))
        if root:
            pt=wintypes.POINT(12,12);geometry.user32.ClientToScreen.argtypes=[wintypes.HWND,ctypes.POINTER(wintypes.POINT)];assert geometry.user32.ClientToScreen(hwnd,ctypes.byref(pt));x,y=pt.x,pt.y
        else:x,y=rect.left+85,rect.top+65
        assert rect.left<=x<rect.right and rect.top<=y<rect.bottom,'Owned selection coordinates outside target'
        inp(c,"move_to",user=user,x=x,y=y)
        for down in(True,False):inp(c,"button",user=user,button="primary",pressed=down)
        s=helper.eventually(lambda:state(c),lambda v:v["users"][user]["keyboard"]["clicked"]and v["users"][user]["keyboard"]["clicked"]["hwnd"]==hwnd,8)
        current={u["overlay"]["hwnd"]for u in s["users"]};assert len(current)==len(s['users'])and all(h and helper.user32.IsWindow(h)for h in current);layers.update(current)
        return s
    def start(c,ts,names):
        return call(c,"routing_start",{"users":names,"user_ids":["restart-"+n for n in names],"allowed_pids":[t["process"].pid for t in ts],"mode":"concurrent","multi_keyboard":True})
    def settings(c,user,follow):
        candidate=copy.deepcopy(next(v["settings"]for v in state(c)["user_settings"]if v["user"]==user));candidate["keyboard"]={"enabled":True,"focus":"clicked"};candidate["follow_application"]=follow
        value=call(c,"routing_settings",{"user":user,"settings":candidate});assert next(v["settings"]for v in value["user_settings"]if v["user"]==user)==candidate;return value
    def events(t,vk):
        r=report(t)
        return [x for x in r["key_events"]if x["vk"]==vk]if t["pump"]else[x for x in r["native_events"]if x["kind"]=="key"and x["code"]==vk]
    def secondary(t):return[x for x in report(t)['native_events']if x['kind']=='button'and x['code']==1]
    def pair(c,user,t,k,vk,hwnd):
        before=len(events(t,vk))
        for down in(True,False):key(c,user,k,down)
        helper.eventually(lambda:events(t,vk),lambda v:len(v)>=before+2,8)
        delta=events(t,vk)[before:];assert[x["pressed"]for x in delta]==[True,False]
        if not t["pump"]:assert all(x["hwnd"]==hwnd for x in delta)
        return delta
    def stop(c,ts,error=False):
        result=call(c,"routing_stop",error=error)
        if not error:
            helper.eventually(lambda:state(c),lambda s:not s["running"]and s["recovery"]["phase"]=="stopped"and s["recovery"]["can_start"],10)
            eligible(c,ts,{"reusable"})
        return result
    def deny_attach(ts,target,label,busy=False):
        c=connect(label);before=signature(ts);ready=inventory(c,ts)
        assert all(v["state"]=="busy"for v in ready.values())if busy else all(v["state"]not in("fresh","reusable")for v in ready.values())
        start(c,ts,[label]);rect=geometry.Rect();assert geometry.user32.GetWindowRect(target["child"],ctypes.byref(rect))
        assert rect.right-rect.left>85 and rect.bottom-rect.top>65
        inp(c,"move_to",user=0,x=rect.left+85,y=rect.top+65)
        reply=call(c,"routing_input",{"action":"button","user":0,"button":"primary","pressed":True})
        expected_status='Routing unavailable: '+ready[str(target['process'].pid)]['reason']
        reply=helper.eventually(lambda:state(c),lambda s:len(s['users'])==1 and s['users'][0]['status']==expected_status,8)
        assert reply['running']and reply['users'][0]['buttons']==[]and reply['users'][0]['keys']==[]and not reply['users'][0]['captured']
        quiet(ts);assert signature(ts)==before,"Denied competitor emitted native input"
        end=state(c);call(c,"routing_stop");c.close()
        return {"before":ready,"error":reply,"after":end,"native_unchanged":True}
    def worker(c):
        cmd=f"@(Get-CimInstance Win32_Process | Where-Object {{ $_.ParentProcessId -eq {c.process.pid} -and $_.CommandLine -like '*session-worker*' }} | Select-Object ProcessId,ParentProcessId,ExecutablePath,CommandLine,CreationDate) | ConvertTo-Json -Compress"
        def query():
            result=subprocess.run(["powershell","-NoProfile","-Command",cmd],capture_output=True,text=True,timeout=10);assert result.returncode==0
            rows=json.loads(result.stdout)if result.stdout.strip()else[]
            if isinstance(rows,dict):rows=[rows]
            return rows
        rows=helper.eventually(query,lambda rs:len(rs)==1,10);w=rows[0]
        assert Path(w["ExecutablePath"]).resolve()==images[64].resolve()and str(c.process.pid)in w["CommandLine"]and w["ParentProcessId"]==c.process.pid
        handle=kernel.OpenProcess(0x100000|0x1000,False,w['ProcessId']);assert handle
        worker_handles[w['ProcessId']]=handle;workers.append(w);return w
    def worker_exit(w,expected=0):
        handle=worker_handles[w['ProcessId']];assert kernel.WaitForSingleObject(handle,10000)==0
        code=wintypes.DWORD();assert kernel.GetExitCodeProcess(handle,ctypes.byref(code));w['observed_exit_code']=code.value
        assert code.value==expected;save();return code.value
    try:
        archive=run/"runner-source";archive.mkdir()
        for name in(Path(__file__).name,"test-mcp.py","test-output-shutdown-timeout.py","owned-gui-geometry.py"):shutil.copy2(Path(__file__).with_name(name),archive/name)
        save()
        if a.phase=="native":
            c=connect("main");a64=spawn("original-a",64,True);b32=spawn("original-b",32);ts=[a64,b32]
            initial=eligible(c,ts,{"fresh"});start(c,ts,["A-first","B-first"]);s1focus=[focus(c,i,t)for i,t in enumerate(ts)];w1=worker(c)
            settings(c,0,True)
            first_observer=helper.eventually(lambda:state(c),lambda s:(s["users"][0]["overlay"].get("application_cursor")or{}).get("state")=="Following application cursor",10)
            key(c,0,"f6",True);inp(c,"button",user=0,button="secondary",pressed=True);key(c,1,"f7",True)
            helper.eventually(lambda:[events(a64,117),events(b32,118)],lambda rs:all(len(v)==1 and v[0]["pressed"]for v in rs),8)
            helper.eventually(lambda:secondary(a64),lambda rs:len(rs)==1 and rs[0]['pressed']and rs[0]['hwnd']==a64['child'],8)
            # An independent GUI owner + standalone competitor gates actual Busy in
            # the GUI phase. Two simultaneous standalone guards conflict on F11.
            first_stop=stop(c,ts)
            worker_exit(w1)
            helper.eventually(lambda:[events(a64,117),events(b32,118)],lambda rs:all([x["pressed"]for x in v]==[True,False]for v in rs),8)
            assert events(a64,117)[1]["hwnd"]==a64["child"]and events(b32,118)[1]["hwnd"]==b32["child"]
            assert[(x['pressed'],x['hwnd'])for x in secondary(a64)]==[(True,a64['child']),(False,a64['child'])]
            ready1=inventory(c,ts);assert all(ready1[k]['creation']==v['creation']for k,v in initial.items())
            after_stop=signature(ts);quiet(ts);assert signature(ts)==after_stop
            start(c,ts,["B-second","A-second"]);w2=worker(c);assert w2["ProcessId"]!=w1["ProcessId"]
            default2=state(c);assert all(not x["settings"]["follow_application"]for x in default2["user_settings"])
            s2focus=[focus(c,0,b32,True),focus(c,1,a64,True)];settings(c,0,False);settings(c,1,True)
            observer2=helper.eventually(lambda:state(c),lambda s:(s["users"][1]["overlay"].get("application_cursor")or{}).get("state")=="Following application cursor"and s["users"][1]["overlay"]["application_cursor"]["hwnd"]==a64["root"],10)
            assert observer2["users"][1]["overlay"]["application_cursor"]["pid"]==a64["process"].pid
            assert len(events(a64,117))==2 and len(events(b32,118))==2
            new_pairs=[pair(c,0,b32,"f8",119,b32["root"]),pair(c,1,a64,"f9",120,a64["root"])]
            second_stop=stop(c,ts)
            worker_exit(w2)
            ready2=inventory(c,ts);assert all(ready2[k]['creation']==v['creation']for k,v in initial.items())
            assert[(x['pressed'],x['hwnd'])for x in secondary(a64)]==[(True,a64['child']),(False,a64['child'])]
            e["groups"]["same_original_two_sessions"]={"original_creation":initial,"ready_after_stop1":ready1,"ready_after_stop2":ready2,"session1_focus":s1focus,"session1_observer":first_observer,"first_stop":first_stop,"second_default_state":default2,"second_focus":s2focus,"second_observer":observer2,"new_pairs":new_pairs,"second_stop":second_stop,"worker1":w1,"worker2":w2,"reports":[report(t)for t in ts],"source_device_change":"Per-user target identity reassignment only; both native processes accept the same global device set. Old-ID rejection UNPROVEN natively."};save()
            c.close()
            for t in ts:close_target(t)
        # Allfences precedePrepare: this is genuine whole-session failure, notforcedpartialPrepare.
        c=connect("fence-owner");healthy=spawn("fence-healthy");blocked=spawn("fence-blocked",is_pump=True);ts=[healthy,blocked]
        start(c,ts,["healthy","blocked"])
        for i,t in enumerate(ts):focus(c,i,t)
        key(c,0,"f6",True);key(c,1,"f7",True)
        helper.eventually(lambda:[events(healthy,117),events(blocked,118)],lambda rs:all(len(v)==1 and v[0]["pressed"]for v in rs),8)
        pump.put(blocked["report"].with_suffix(".mode.json"),{"mode":"none","milliseconds":3500})
        helper.eventually(lambda:report(blocked),lambda r:r["pump_mode"]=="none"and r["mode_entered"]==1,5)
        failure=stop(c,ts,True)
        resumed=helper.eventually(lambda:report(blocked),lambda r:r["pump_mode"]=="normal",8);quiet(ts)
        nonreuse=eligible(c,ts,{"dirty","busy"})
        attempts=[deny_attach(ts,t,"poison-target-"+str(i))for i,t in enumerate(ts)]
        close_target(blocked);still=inventory(c,[healthy]);assert all(v["state"]not in("fresh","reusable")for v in still.values())
        e["groups"]["failed_all_target_cleanup"]={"failure":failure,"pump_resumed_report":resumed,"late_events_in_reports_not_cleanup_success":True,"nonreuse":nonreuse,"fresh_controller_denials":attempts,"healthy_still_blocked_after_other_exit":still,"partial_prepare_native":"UNPROVEN;allfencesarebeforeprepare","reports":[report(t)for t in ts]};save();close_target(healthy);c.close()
        c=connect("worker-owner");lost=spawn("worker-loss");start(c,[lost],["worker-loss"]);focus(c,0,lost);key(c,0,"f6",True)
        helper.eventually(lambda:events(lost,117),lambda rs:len(rs)==1 and rs[0]["pressed"],8);w=worker(c)
        # Own exactchildonly; psutil identity/creation guards arecheckedbeforedeliberatekill.
        import psutil
        victim=psutil.Process(w["ProcessId"]);born=victim.create_time();assert victim.ppid()==c.process.pid and Path(victim.exe()).resolve()==images[64].resolve()and "session-worker"in victim.cmdline()
        assert victim.create_time()==born;victim.kill();code=victim.wait(timeout=10);assert code!=0
        failed=helper.eventually(lambda:state(c),lambda s:not s["running"]and s["recovery"]["phase"]=="quarantined",10)
        denial=deny_attach([lost],lost,"after-worker-loss")
        assert lost["process"].poll()is None
        e["groups"]["worker_loss"]={"worker":w,"worker_creation":born,"expected_nonzero_exit":code,"controller_state":failed,"denial":denial,"native_before_target_exit":report(lost)};save()
        close_target(lost);helper.eventually(lambda:state(c),lambda s:s["recovery"]["can_start"],10);c.close()
        e["result"]="PENDING INDEPENDENT NATIVE DATA REVIEW";save()
    except Exception as error:e["failure"]=repr(error);save();raise
    finally:
        errors=[]
        for c in clients:
            try:c.close()
            except Exception as error:
                errors.append(repr(error))
                if c.process.poll()is None:c.process.kill();c.process.wait(timeout=10)
                c.stderr.close()
        for t in targets:
            try:close_target(t)
            except Exception as error:
                errors.append(repr(error))
                if t["process"].poll()is None:t["process"].terminate();t["process"].wait(timeout=10)
        for log in logs:log.close()
        for pid,handle in worker_handles.items():
            code=wintypes.DWORD();kernel.GetExitCodeProcess(handle,ctypes.byref(code))
            e.setdefault('worker_terminal_codes',{})[str(pid)]=code.value;kernel.CloseHandle(handle)
            if code.value==259:errors.append('Owned worker still alive: '+str(pid))
        owned_pids=[c.process.pid for c in clients]+[t['process'].pid for t in targets]+[w['ProcessId']for w in workers]
        paths=','.join("'"+str(q.resolve()).replace("'","''")+"'"for q in images.values())
        audit=subprocess.run(['powershell','-NoProfile','-Command',f"@(Get-CimInstance Win32_Process | Where-Object {{ $_.ProcessId -in @({','.join(map(str,owned_pids))}) -or $_.ExecutablePath -in @({paths}) }} | Select-Object ProcessId,ParentProcessId,ExecutablePath) | ConvertTo-Json -Compress"],capture_output=True,text=True,timeout=20)
        assert audit.returncode==0,audit.stderr
        e['scoped_cim_survivors']=json.loads(audit.stdout)if audit.stdout.strip()else[]
        if e['scoped_cim_survivors']:errors.append('Owned process survived cleanup')
        e["cleanup_errors"]=errors;e["owned_exit_codes"]={str(c.process.pid):c.process.returncode for c in clients}|{str(t["process"].pid):t["process"].returncode for t in targets};e["known_overlays_closed"]=bool(layers)and all(not helper.user32.IsWindow(h)for h in layers);e["worker_identities"]=workers
        if errors:e["result"]="NOT ACCEPTED"
        save();print(e["result"]+": "+str(run),flush=True)


def gui(a,p):
    lifecycle=module('restart_gui_base','test-app-lifecycle-ui.py');strict=module('restart_gui_exact','test-keyboard-persistence-ui.py');mcp=module('restart_gui_mcp','test-mcp.py')
    Base,ui,hotkeys,geometry=lifecycle.gate_type()
    class Gate(Base):
        def __init__(self):
            super().__init__(a,'clean-restart-gui',offline=False,count=1)
            self.target=None;self.root=None;self.layers=set();self.expected=None;self.holds={};self.originals={};self.competitor=None;self.competitor_worker=None;self.competitor_layers=set()
        def probes(self):return ui.eventually(lambda:[ui.read_json(self.run/'probe-0.json')],lambda v:True,3)
        def snapshot(self):return ui.eventually(lambda:super(Gate,self).snapshot(),lambda v:True,3)
        def frame(self,name,size):
            hwnd=self.main_hwnd()
            if geometry.geometry(hwnd)['client_logical']!=list(map(float,size)):
                geometry.resize_owned_client(hwnd,size);return False
            hold=self.holds.setdefault(name,{'started':time.time(),'writes':[]});grace=hold['started']+3.2
            if time.time()<grace:return False
            try:
                from PIL import Image
                source=self.run/'ui.png';stat=source.stat()
                if stat.st_mtime<=grace:return False
                raw=source.read_bytes();picture=Image.open(io.BytesIO(raw));picture.load()
                if picture.size!=tuple(size)or source.stat().st_mtime_ns!=stat.st_mtime_ns:return False
                write=[stat.st_mtime_ns,len(raw),hashlib.sha256(raw).hexdigest()]
                if not hold['writes']or write[0]!=hold['writes'][-1][0]:hold['writes'].append(write)
                if len(hold['writes'])<2:return False
                original=geometry.copy_owned_product_frame(source,self.run/(name+'.png'),hwnd,grace)
                if not original:return False
                self.originals[name]=original;self.evidence['originals']=self.originals;self.evidence['capture_holds']=self.holds;return True
            except(OSError,ValueError):return False
        def close_target(self):
            if self.target and self.target.poll()is None:
                assert geometry.user32.PostMessageW(self.root,0x10,0,0);self.target.wait(timeout=8)
            if self.target:assert self.target.returncode==0
        def busy_competitor(self):
            try:
                before=self.probes()[0];native_before=before['native_events']
                c=mcp.Client(self.images[0],a.runtime.resolve(),self.run/'busy-competitor.stderr.log');self.competitor=c;self.owned.append(c.process)
                calls=[];self.evidence['gui_busy_calls']=calls
                def call(name,args=None,error=False):
                    item={'tool':name,'arguments':args or {},'expected_error':error};calls.append(item);self.save()
                    item['result']=c.tool(name,args,error=error)
                    if name=='routing_windows':item['result']['windows']=[w for w in item['result']['windows']if w['pid']==self.target.pid]
                    self.save();return item['result']
                windows=call('routing_windows')['windows'];assert windows and all(w['eligibility']['state']=='busy'for w in windows)
                call('routing_start',{'users':['busy-competitor'],'user_ids':['busy-only'],'allowed_pids':[self.target.pid],'mode':'concurrent','multi_keyboard':True})
                started=ui.eventually(lambda:call('routing_state'),lambda s:len(s['users'])==1 and bool(s['users'][0]['overlay']['hwnd']),10)
                self.competitor_layers={u['overlay']['hwnd']for u in started['users']};assert len(self.competitor_layers)==1 and all(ui.user32.IsWindow(h)for h in self.competitor_layers)
                query=f"@(Get-CimInstance Win32_Process | Where-Object {{ $_.ParentProcessId -eq {c.process.pid} -and $_.CommandLine -like '*session-worker*' }} | Select-Object ProcessId,ParentProcessId,ExecutablePath,CommandLine) | ConvertTo-Json -Compress"
                def worker_rows():
                    result=subprocess.run(['powershell','-NoProfile','-Command',query],capture_output=True,text=True,timeout=10);assert result.returncode==0
                    rows=json.loads(result.stdout)if result.stdout.strip()else[];return[rows]if isinstance(rows,dict)else rows
                rows=ui.eventually(worker_rows,lambda rs:len(rs)==1,10);w=rows[0];assert Path(w['ExecutablePath']).resolve()==self.images[0].resolve()and str(c.process.pid)in w['CommandLine'];self.competitor_worker=w
                kernel=ctypes.WinDLL('kernel32',use_last_error=True);kernel.OpenProcess.argtypes=[wintypes.DWORD,wintypes.BOOL,wintypes.DWORD];kernel.OpenProcess.restype=wintypes.HANDLE;kernel.WaitForSingleObject.argtypes=[wintypes.HANDLE,wintypes.DWORD];kernel.GetExitCodeProcess.argtypes=[wintypes.HANDLE,ctypes.POINTER(wintypes.DWORD)];kernel.CloseHandle.argtypes=[wintypes.HANDLE]
                handle=kernel.OpenProcess(0x100000|0x1000,False,w['ProcessId']);assert handle
                try:
                    rect=geometry.Rect();child=before['capture_hwnd'];assert geometry.user32.GetWindowRect(child,ctypes.byref(rect));assert rect.right-rect.left>85 and rect.bottom-rect.top>65
                    call('routing_input',{'action':'move_to','user':0,'x':rect.left+85,'y':rect.top+65})
                    denied=call('routing_input',{'action':'button','user':0,'button':'primary','pressed':True})
                    expected_status='Routing unavailable: '+windows[0]['eligibility']['reason']
                    denied=ui.eventually(lambda:call('routing_state'),lambda s:len(s['users'])==1 and s['users'][0]['status']==expected_status,8)
                    assert denied['running']and denied['users'][0]['buttons']==[]and denied['users'][0]['keys']==[]and not denied['users'][0]['captured']
                    later=ui.eventually(self.probes,lambda rs:rs[0]['frames']>=before['frames']+3,8)[0]
                    native_after=later['native_events'];assert native_after==native_before
                    call('routing_stop');assert kernel.WaitForSingleObject(handle,10000)==0;code=wintypes.DWORD();assert kernel.GetExitCodeProcess(handle,ctypes.byref(code))and code.value==0;w['observed_exit_code']=code.value
                    c.close();assert c.process.returncode==0
                    assert all(not ui.user32.IsWindow(h)for h in self.competitor_layers)
                    result={'readiness':windows,'actual_denial':denied,'native_before':native_before,'native_after':native_after,'worker':w,'competitor_exit_code':c.process.returncode,'mutation_calls':4,'routing_input_calls':2,'owner_parent_alive':self.target.poll()is None};self.evidence['gui_busy_result']=result;self.save();return result
                finally:kernel.CloseHandle(handle)
            except Exception as error:raise RuntimeError('Imperative GUI competitor cannot be retried')from error
    g=Gate();prefs={'version':1,'appearance_mode':'dark','zoom_percent':100,'launch_minimized':False,'minimize_to_tray':False,'close_to_tray':False,'launch_at_login':False,'shortcuts':[]}
    try:
        g.target=subprocess.Popen([str(g.images[0]),'probe','--automation','--report',str(g.run/'probe-0.json')],creationflags=subprocess.CREATE_NO_WINDOW);g.owned.append(g.target)
        g.root=ui.eventually(lambda:geometry.owned_window(g.target.pid),bool,10);ui.eventually(g.probes,lambda rs:rs[0]['frames']>2,8)
        assert geometry.user32.SetWindowPos(g.root,wintypes.HWND(-1),30,40,420,360,0x10)
        profile={'version':1,'concurrent':True,'desktop':None,'seats':[{'id':'clean-restart-gui','name':'Restart Alpha','mouse':'virtual://mouse/41','keyboard':'virtual://keyboard/41','target_pid':g.target.pid,'cursor':{'lock_window':False}}]}
        steps=[ui.step('start1','Start routing'),ui.step('stop1','Stop routing'),ui.step('start2','Start routing'),ui.step('stop2','Stop routing')]
        def verify(name,sample):
            strict.strict_equal(ui.read_json(g.preferences_path),prefs)
            if g.expected is None:g.expected=copy.deepcopy(sample['profile_state']['profile'])
            strict.strict_equal(sample['profile_state']['profile'],g.expected)
            assert g.target.poll()is None
            if name.startswith('start'):
                if not sample['state']:return False
                layers={r['overlay']['hwnd']for r in sample['state']};assert len(layers)==1 and all(h and ui.user32.IsWindow(h)for h in layers);g.layers.update(layers)
                if name=='start1':g.once('actual-busy-competitor',g.busy_competitor)
            else:
                recovery=ui.read_json(g.run/'ui.recovery.json')
                if recovery['phase']!='stopped'or not recovery['can_start']:return False
                if any(ui.user32.IsWindow(h)for h in g.layers):return False
                assert not any(r['held_keys']or r['buttons']for r in g.probes())
                g.evidence.setdefault('stop_recovery',{})[name]=recovery
                if not g.frame(name,[360,360]if name=='stop1'else[320,360]):return False
            return True
        def after(gate):
            gate.close_target();gate.expected_exit=True;assert geometry.user32.PostMessageW(gate.main_hwnd(),0x10,0,0);gate.gui.wait(timeout=10)
            assert gate.gui.returncode==0 and not hotkeys.lifecycle_window(gate.gui.pid);return True
        g.evidence.update(producer=str(a.producer),producer_sha256=digest(a.producer),scope='Four real Start/Stop receipts plus four standalone competitor mutations/two virtual selection calls rejected before native delivery; two Home readability originals, readiness reason only if actually visible.')
        archive=g.run/'runner-source';archive.mkdir()
        for name in(Path(__file__).name,'test-app-lifecycle-ui.py','test-keyboard-persistence-ui.py','owned-gui-geometry.py','test-mcp.py'):shutil.copy2(Path(__file__).with_name(name),archive/name)
        ui.put(archive/'all-dependency-hashes.json',{q.name:digest(q)for q in Path(__file__).parent.glob('*.py')})
        g.execute(profile,prefs,steps,verify,after_controls=after);g.evidence['result']='PENDING INDEPENDENT DATA/PIXEL REVIEW'
    finally:
        if g.competitor:
            try:g.competitor.close()
            except Exception as error:g.evidence.setdefault('cleanup_errors',[]).append(repr(error))
        try:g.close_target()
        except Exception as error:g.evidence.setdefault('cleanup_errors',[]).append(repr(error))
        g.close();lifecycle.remove_startup_key(g.startup_key)
        g.evidence['owned_exit_codes']={str(q.pid):q.returncode for q in g.owned};g.evidence['guid_value_absent']=lifecycle.startup_value(g.startup_key)is None
        g.evidence['descriptor_absent']=g.gui is None or not list(Path(tempfile.gettempdir()).glob(f'protoinputmultiplex-agent-{g.gui.pid}-*.json'))
        g.evidence['known_overlays_closed']=bool(g.layers)and all(not ui.user32.IsWindow(h)for h in g.layers)
        g.evidence['competitor_overlays_closed']=bool(g.competitor_layers)and all(not ui.user32.IsWindow(h)for h in g.competitor_layers)
        pids=','.join(str(q.pid)for q in g.owned)+(','+str(g.competitor_worker['ProcessId'])if g.competitor_worker else'')
        audit=subprocess.run(['powershell','-NoProfile','-Command',f"@(Get-CimInstance Win32_Process | Where-Object {{ $_.ProcessId -in @({pids}) }} | Select-Object ProcessId,ExecutablePath) | ConvertTo-Json -Compress"],capture_output=True,text=True,timeout=20);assert audit.returncode==0
        g.evidence['scoped_cim_survivors']=json.loads(audit.stdout)if audit.stdout.strip()else[]
        if g.evidence.get('cleanup_errors')or g.evidence['scoped_cim_survivors']or not g.evidence['guid_value_absent']or not g.evidence['descriptor_absent']or not g.evidence['known_overlays_closed']or not g.evidence['competitor_overlays_closed']:g.evidence['result']='NOT ACCEPTED'
        g.save()


def main():
    parser=argparse.ArgumentParser();parser.add_argument("--ready",action="store_true");parser.add_argument("--phase",choices=("native","native-tail","gui"),required=True)
    for name in("executable","x86-executable","runtime","producer"):parser.add_argument("--"+name,type=Path,required=True)
    parser.add_argument("--producer-sha256",required=True);parser.add_argument("--from-run",type=Path);a=parser.parse_args();p=freeze(a)
    (native if a.phase in("native","native-tail")else gui)(a,p)


if __name__=="__main__":main()
