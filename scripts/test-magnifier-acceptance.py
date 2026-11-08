"""Finite virtual-only magnifier acceptance; Desktop pixels stay inside an owned EDIT.

Do not launch before the parent declares both binaries/runtime frozen and grants
the exclusive live slot. PNGs are decoded independently; no hardware/global input.
On redirected-surface hosts, --large-frame validates the real native/pixel job
boundary. The default print-delay criterion intentionally rejects unexercised callbacks.
"""
import argparse
import copy
import ctypes
from ctypes import wintypes
import importlib.util
from pathlib import Path
import time
from PIL import Image

_spec=importlib.util.spec_from_file_location("annotation_gate",Path(__file__).with_name("test-annotation-acceptance.py"))
_module=importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_module)
eventually,user32,Rect=_module.eventually,_module.user32,_module.Rect
user32.GetWindowDisplayAffinity.argtypes=[wintypes.HWND,ctypes.POINTER(wintypes.DWORD)]
user32.PostMessageW.argtypes=[wintypes.HWND,wintypes.UINT,ctypes.c_size_t,ctypes.c_ssize_t]
user32.GetWindow.argtypes=[wintypes.HWND,wintypes.UINT]
user32.GetWindow.restype=wintypes.HWND
user32.SetWindowLongPtrW.argtypes=[wintypes.HWND,ctypes.c_int,ctypes.c_ssize_t]
user32.SetWindowLongPtrW.restype=ctypes.c_ssize_t
user32.SetLayeredWindowAttributes.argtypes=[wintypes.HWND,wintypes.DWORD,wintypes.BYTE,wintypes.DWORD]


class MagnifierGate(_module.AnnotationGate):
    run_prefix="acceptance-magnifier"
    tool_names=_module.AnnotationGate.tool_names|{"magnifier"}

    def before_selection(self):
        pass

    def magnifier(self,operation="inspect",**fields):
        return self.client.tool("magnifier",{"operation":operation,**fields})["magnifier"]

    def configure_view(self,user,**changes):
        settings=copy.deepcopy(self.view_defaults)
        settings.update(changes)
        state=self.magnifier("configure",user=user,settings=settings)
        assert state["error"] is None,state
        assert state["users"][user]["settings"]==settings,(settings,state)
        return settings

    def frame(self,user,after=0):
        return eventually(lambda:self.magnifier()["users"][user],lambda u:u["capture_sequence"]>after and u["overlay"] and u["overlay"]["visible"] and not u["in_flight"],20)

    def refreshed(self,user):
        before=self.magnifier()["users"][user]["capture_sequence"]
        self.magnifier("refresh",user=user)
        return self.frame(user,before)

    def exported_view(self,user,name):
        path=self.run/(name+".png")
        state=self.magnifier("export",user=user,path=str(path))
        assert state["error"] is None,state
        with Image.open(path) as source:
            assert source.mode=="RGBA",source.mode
            image=source.copy()
        info=state["export"]
        assert list(image.size)==[info["width"],info["height"]]
        self.evidence.setdefault("exports",{})[name]=state
        return image

    def native_view(self,user):
        state=self.frame(user)
        overlay=state["overlay"]
        rect=Rect()
        assert user32.GetWindowRect(overlay["hwnd"],ctypes.byref(rect))
        actual={k:getattr(rect,k) for k in ("left","top","right","bottom")}
        assert actual==overlay["bounds"],(actual,overlay)
        styles=user32.GetWindowLongPtrW(overlay["hwnd"],-20)
        assert styles&0x80000 and styles&0x20 and styles&0x8000000
        assert user32.IsWindowVisible(overlay["hwnd"]) and overlay["click_through"] and overlay["no_activate"]
        affinity=wintypes.DWORD()
        assert user32.GetWindowDisplayAffinity(overlay["hwnd"],ctypes.byref(affinity))
        assert affinity.value==0x11 and overlay["excluded_from_capture"],(affinity.value,overlay)
        for message,expected in ((0x84,-1),(0x21,3)):
            result=ctypes.c_size_t()
            assert user32.SendMessageTimeoutW(overlay["hwnd"],message,0,0,2,1000,ctypes.byref(result))
            assert ctypes.c_ssize_t(result.value).value==expected
        self.evidence.setdefault("native_views",[]).append({"user":user,"state":state,"rect":actual,"display_affinity":affinity.value})
        return state

    def discover_view(self):
        self.defaults=self.state()["user_settings"][0]["settings"]
        self.annotation_defaults=self.annotation()["users"][0]["settings"]
        state=self.magnifier()
        self.view_defaults=state["users"][0]["settings"]
        assert self.view_defaults=={"mode":"off","source":"selected_target","zoom":2.0,"width":320,"height":240,"border":True,
                                  "border_color":[96,165,250],"auto_refresh":True,"refresh_hz":8,"monitor_id":None,"work_area":True,"dock":"right","desktop_region":None}
        assert all(u["settings"]["mode"]=="off" and not u["overlay"] for u in state["users"])
        schema=next(t["inputSchema"] for t in self.evidence["tools"] if t["name"]=="magnifier")
        import json
        serialized=json.dumps(schema)
        for token in ("configure","refresh","export","selected_target","desktop","fullscreen","docked","auto_refresh","monitor_id","desktop_region"):
            assert '"'+token+'"' in serialized,token
        displays=self.client.tool("routing_displays")
        self.monitor=next(m for m in displays["monitors"] if m["primary"] and m["stable_id"])
        self.child_bounds=[]
        for i in (0,1):
            hwnd=self.report(i)["capture_hwnd"]
            rect=Rect()
            origin=wintypes.POINT()
            assert user32.GetClientRect(hwnd,ctypes.byref(rect)) and user32.ClientToScreen(hwnd,ctypes.byref(origin))
            self.child_bounds.append({"left":origin.x,"top":origin.y,"right":origin.x+rect.right,"bottom":origin.y+rect.bottom})
        self.snapshot("sixteen-tools-full-magnifier-defaults-and-owned-source-rectangles")

    def validation(self):
        self.key(1,"f5",True)
        eventually(lambda:self.report(1),lambda r:116 in r["held_keys"])
        before=self.magnifier()
        for changes in ({"zoom":.5},{"zoom":33},{"width":31},{"width":4097},{"width":4096,"height":4096},
                        {"refresh_hz":0},{"refresh_hz":31},{"mode":"lens","monitor_id":"not-a-connected-monitor"},
                        {"source":"desktop","desktop_region":None},
                        {"source":"desktop","desktop_region":{"left":10,"top":10,"right":10,"bottom":20}}):
            settings={**self.view_defaults,**changes}
            self.client.tool("magnifier",{"operation":"configure","user":0,"settings":settings},error=True)
            assert self.magnifier()["users"]==before["users"]
            assert self.state()["running"] and 116 in self.report(1)["held_keys"]
        self.key(1,"f5",False)
        self.snapshot("invalid-settings-and-unscoped-desktop-reject-without-state-or-hold-mutation")

    def selected_pixels(self):
        for user in (0,1):
            source=self.child_bounds[user]
            self.go(user,(source["left"]+12,source["top"]+12))
            spans={}
            for zoom in (1,2,4):
                self.configure_view(user,mode="lens",width=96,height=96,zoom=zoom,border=False,auto_refresh=False)
                self.refreshed(user)
                state=self.native_view(user)
                assert state["source_hwnd"]==self.report(user)["capture_hwnd"] and state["source_pid"]==self.probes[user].pid
                image=self.exported_view(user,f"selected-{user}-zoom-{zoom}")
                assert image.size==(96,96)
                spans[zoom]=sum(image.getpixel((x,48))==(255,255,255,255) for x in range(96))
                assert image.getpixel((48,48))==(255,255,255,255)
            assert spans=={1:16,2:32,4:64},spans
            self.go(user,(source["left"]+4,source["top"]+12))
            self.configure_view(user,mode="lens",width=64,height=64,zoom=32,border=False,auto_refresh=False)
            self.refreshed(user)
            image=self.exported_view(user,f"selected-{user}-zoom-32-edge")
            assert image.getpixel((16,32))==(*self.colors[user],255)
            assert image.getpixel((48,32))==(255,255,255,255)
            self.configure_view(user,mode="lens",width=64,height=64,zoom=2,border=True,border_color=[7,29,53],auto_refresh=False)
            self.refreshed(user)
            image=self.exported_view(user,f"selected-{user}-rgb-border")
            for p in ((0,0),(1,32),(32,1),(62,32),(32,62),(63,63)):
                assert image.getpixel(p)==(7,29,53,255)
            assert image.getpixel((2,32))!=(7,29,53,255)
            sequence=self.magnifier()["users"][user]["capture_sequence"]
            time.sleep(.4)
            assert self.magnifier()["users"][user]["capture_sequence"]==sequence,"manual mode captured automatically"
            self.configure_view(user)
        self.snapshot("actual-x64-x86-selected-pixels-zoom-registration-spans-border-manual-stability")

    def placement(self):
        area=self.monitor["work_area"]
        foreground=user32.GetForegroundWindow()
        for side in ("left","right","top","bottom"):
            self.configure_view(0,mode="docked",width=64,height=64,dock=side,monitor_id=self.monitor["id"],work_area=True,auto_refresh=False)
            self.refreshed(0)
            bounds=self.native_view(0)["overlay"]["bounds"]
            assert area["left"]<=bounds["left"]<bounds["right"]<=area["right"]
            assert area["top"]<=bounds["top"]<bounds["bottom"]<=area["bottom"]
            edge={"left":"left","right":"right","top":"top","bottom":"bottom"}[side]
            assert bounds[edge]==area[edge],(side,bounds,area)
        self.configure_view(0,mode="fullscreen",zoom=32,monitor_id=self.monitor["id"],work_area=True,auto_refresh=False)
        self.refreshed(0)
        assert self.native_view(0)["overlay"]["bounds"]==area
        assert self.exported_view(0,"fullscreen-work-area").size==(area["right"]-area["left"],area["bottom"]-area["top"])
        assert user32.GetForegroundWindow()==foreground,"magnifier placement activated a window"
        self.configure_view(0)
        self.snapshot("four-dock-sides-fullscreen-selected-stable-monitor-work-area-native-geometry")

    def cursor_zorder(self):
        self.anchored(0)
        self.apply(0,shape="crosshair")
        for mode,changes in (("lens",{}),("docked",{"dock":"left","width":256,"height":1200})):
            self.configure_view(0,**{"mode":mode,"width":64,"height":64,"auto_refresh":False,"monitor_id":self.monitor["id"],"work_area":True,**changes})
            self.refreshed(0)
            state=self.state()
            cursor=state["users"][0]["overlay"]
            view=state["magnifier"]["users"][0]["overlay"]
            assert cursor["visible"] and user32.IsWindowVisible(cursor["hwnd"])
            rect=Rect()
            assert user32.GetWindowRect(cursor["hwnd"],ctypes.byref(rect))
            assert [rect.left+cursor["hotspot"][0],rect.top+cursor["hotspot"][1]]==[cursor["x"],cursor["y"]]
            above=[]
            hwnd=user32.GetWindow(cursor["hwnd"],3)
            while hwnd and len(above)<500:
                above.append(hwnd)
                hwnd=user32.GetWindow(hwnd,3)
            if mode=="docked":
                assert view["bounds"]["left"]<=cursor["x"]<view["bounds"]["right"] and view["bounds"]["top"]<=cursor["y"]<view["bounds"]["bottom"]
                assert view["hwnd"] not in above,("opaque docked magnifier covers virtual cursor",cursor,view,above)
            self.evidence["checks"]["cursor-native-visible-position-zorder-"+mode]={"state":state,"cursor_rect":[rect.left,rect.top,rect.right,rect.bottom],"above_cursor_hwnds":above}
        self.configure_view(0)

    def fixture_message(self,user,message,value,field,expected):
        hwnd=self.report(user)["capture_hwnd"]
        assert user32.IsWindow(hwnd) and self.report(user)["pid"]==self.probes[user].pid
        assert user32.PostMessageW(hwnd,message,value,0)
        eventually(lambda:self.report(user),lambda r:r[field]==expected,8)

    def set_color(self,user,color):
        self.fixture_message(user,0x8051,color[0]<<16|color[1]<<8|color[2],"capture_color",list(color))

    def set_delay(self,user,delay):
        self.fixture_message(user,0x8052,delay,"capture_delay_ms",delay)

    def dynamic_refresh(self):
        for user in (0,1):
            self.anchored(user)
            self.configure_view(user,mode="lens",width=64,height=64,border=False,auto_refresh=False)
            initial=self.refreshed(user)
            before=self.exported_view(user,f"manual-{user}-before")
            color=(40+user*70,130+user*20,60+user*50)
            self.set_color(user,color)
            time.sleep(.5)
            current=self.magnifier()["users"][user]
            assert current["capture_sequence"]==initial["capture_sequence"]
            assert self.exported_view(user,f"manual-{user}-frozen").tobytes()==before.tobytes()
            self.refreshed(user)
            assert self.exported_view(user,f"manual-{user}-refreshed").getpixel((32,32))==(*color,255)
            self.configure_view(user,mode="lens",width=64,height=64,border=False,auto_refresh=True,refresh_hz=8)
            self.frame(user)
            sequence=self.magnifier()["users"][user]["capture_sequence"]
            color=(130+user*20,40+user*50,170-user*30)
            self.set_color(user,color)
            self.frame(user,sequence)
            # A frame can have started immediately before the pattern update;
            # one later sequence establishes actual new source content.
            self.frame(user,self.magnifier()["users"][user]["capture_sequence"])
            assert self.exported_view(user,f"automatic-{user}-new-pixels").getpixel((32,32))==(*color,255)
            self.configure_view(user)
        self.snapshot("real-pattern-change-manual-freezes-until-refresh-auto-updates-x64-x86")

    def fullscreen_cursor(self):
        self.anchored(0)
        self.anchored(1)
        for user in (0,1):
            self.apply(user,shape="crosshair")
            self.configure_view(user,mode="fullscreen",zoom=2,monitor_id=self.monitor["id"],work_area=True,auto_refresh=False)
            self.refreshed(user)
        state=eventually(self.state,lambda s:s["magnifier"]["users"][1]["overlay"]["visible"] and not s["magnifier"]["users"][0]["overlay"]["visible"])
        assert "Waiting for display" in state["magnifier"]["users"][0]["status"]
        self.input("motion",user=0,dx=3,dy=0)
        state=eventually(self.state,lambda s:s["magnifier"]["users"][0]["overlay"]["visible"] and not s["magnifier"]["users"][1]["overlay"]["visible"]
                         and s["magnifier"]["users"][0]["frame_cursor"]==s["magnifier"]["users"][0]["cursor"])
        view=state["magnifier"]["users"][0]
        output=view["overlay"]["bounds"]
        cursor=state["users"][0]["overlay"]
        assert view["cursor"]==[self.points[0][0]+3,self.points[0][1]]
        assert [cursor["x"],cursor["y"]]==view["cursor"]
        eventually(lambda:self.report(0)["cursor"],lambda point:point==view["cursor"])
        # The active cursor is at its camera anchor: its visual hotspot is the
        # viewport center, while the logical/native target pointer stays above.
        rect=Rect()
        assert user32.GetWindowRect(cursor["hwnd"],ctypes.byref(rect))
        assert [rect.left+cursor["hotspot"][0],rect.top+cursor["hotspot"][1]]==[(output["left"]+output["right"])//2,(output["top"]+output["bottom"])//2]
        assert [rect.left,rect.top]==cursor["render_origin"]
        self.evidence["checks"]["fullscreen-last-active-winner-and-native-cursor-projection"]={"state":state,"cursor_rect":[rect.left,rect.top,rect.right,rect.bottom]}
        for user in (0,1):
            self.configure_view(user)
            self.apply(user)

    def desktop_source(self):
        windows=self.client.tool("routing_windows")["windows"]
        roots=[next(w for w in windows if w["pid"]==p.pid and w["title"].startswith("ProtoInputMultiPlex Automation Test")) for p in self.probes]
        source=self.child_bounds[0]
        region={"left":source["left"]+60,"top":source["top"]+40,"right":source["left"]+188,"bottom":source["top"]+136}
        anchor=((region["left"]+region["right"])//2,(region["top"]+region["bottom"])//2)
        assert source["left"]<=region["left"]<region["right"]<=source["right"]
        assert source["top"]<=region["top"]<region["bottom"]<=source["bottom"]
        for user in (0,1):
            self.apply(user,visible=False)
        self.go(0,anchor)
        self.configure_view(0,mode="lens",source="selected_target",width=64,height=64,border=False,auto_refresh=False)
        self.refreshed(0)
        a=tuple(self.report(0)["capture_color"])
        b=tuple(self.report(1)["capture_color"])
        assert self.exported_view(0,"target-before-overlap").getpixel((32,32))==(*a,255)
        assert user32.SetWindowPos(roots[1]["hwnd"],wintypes.HWND(-1),30,40,420,360,0x10)
        self.refreshed(0)
        assert self.exported_view(0,"target-under-owned-overlap").getpixel((32,32))==(*a,255)
        self.configure_view(0,mode="lens",source="desktop",desktop_region=region,width=64,height=64,border=False,auto_refresh=False)
        self.refreshed(0)
        self.native_view(0)
        assert self.exported_view(0,"desktop-owned-overlap").getpixel((32,32))==(*b,255)
        state=self.magnifier()["users"][0]
        assert state["source_hwnd"] is None and state["source_pid"] is None
        bounded=state["source_bounds"]
        assert region["left"]<=bounded["left"]<bounded["right"]<=region["right"]
        assert region["top"]<=bounded["top"]<bounded["bottom"]<=region["bottom"]
        assert user32.SetWindowPos(roots[1]["hwnd"],wintypes.HWND(-1),490,40,420,360,0x10)
        self.configure_view(0,mode="fullscreen",source="desktop",desktop_region=region,zoom=32,border=False,auto_refresh=False,monitor_id=self.monitor["id"],work_area=True)
        self.refreshed(0)
        view=self.native_view(0)
        assert view["overlay"]["bounds"]["left"]<=region["left"] and view["overlay"]["bounds"]["right"]>=region["right"]
        new_color=(37,149,211)
        self.set_color(0,new_color)
        self.refreshed(0)
        image=self.exported_view(0,"desktop-no-feedback-under-fullscreen")
        assert image.getpixel((image.width//2,image.height//2))==(*new_color,255)
        for routing in self.state()["users"]:
            affinity=wintypes.DWORD()
            assert user32.GetWindowDisplayAffinity(routing["overlay"]["hwnd"],ctypes.byref(affinity)) and affinity.value==0x11
        sequence=self.magnifier()["users"][0]["capture_sequence"]
        self.input("motion",user=0,dx=5,dy=0)
        time.sleep(.3)
        assert self.magnifier()["users"][0]["capture_sequence"]==sequence
        assert self.exported_view(0,"manual-desktop-content-frozen-after-cursor-move").tobytes()==image.tobytes()
        self.configure_view(0)
        for user in (0,1):
            self.apply(user)
        self.snapshot("scoped-true-desktop-vs-selected-hwnd-overlap-fullscreen-feedback-exclusion-manual-freeze")

    def layered_source(self):
        roots=[next(w for w in self.client.tool("routing_windows")["windows"] if w["pid"]==p.pid and w["title"].startswith("ProtoInputMultiPlex Automation Test")) for p in self.probes]
        source=self.child_bounds[0]
        region={"left":source["left"]+60,"top":source["top"]+40,"right":source["left"]+188,"bottom":source["top"]+136}
        anchor=((region["left"]+region["right"])//2,(region["top"]+region["bottom"])//2)
        for user in (0,1):
            self.apply(user,visible=False)
        self.go(0,anchor)
        hwnd=roots[1]["hwnd"]
        old_style=user32.GetWindowLongPtrW(hwnd,-20)
        affinity=wintypes.DWORD()
        assert user32.GetWindowDisplayAffinity(hwnd,ctypes.byref(affinity)) and affinity.value==0,"own source fixture must not be capture-excluded"
        user32.SetWindowLongPtrW(hwnd,-20,old_style|0x80000)
        assert user32.GetWindowLongPtrW(hwnd,-20)&0x80000
        assert user32.SetWindowPos(hwnd,wintypes.HWND(-1),30,40,420,360,0x10)
        checks=[]
        try:
            a=tuple(self.report(0)["capture_color"])
            b=tuple(self.report(1)["capture_color"])
            for alpha in (255,128):
                assert user32.SetLayeredWindowAttributes(hwnd,0,alpha,2)
                time.sleep(.2)
                self.configure_view(0,mode="lens",width=64,height=64,border=False,auto_refresh=False)
                self.refreshed(0)
                selected=self.exported_view(0,f"selected-under-layered-alpha-{alpha}").getpixel((32,32))
                assert selected==(*a,255)
                self.configure_view(0,mode="lens",source="desktop",desktop_region=region,width=64,height=64,border=False,auto_refresh=False)
                self.refreshed(0)
                pixel=self.exported_view(0,f"desktop-layered-alpha-{alpha}").getpixel((32,32))
                expected=tuple((top*alpha+base*(255-alpha)+127)//255 for base,top in zip(a,b))+(255,)
                checks.append({"alpha":alpha,"expected":expected,"desktop_pixel":pixel,"selected_pixel":selected,"source_layer_hwnd":hwnd})
                self.evidence["layered_source_progress"]=checks
                self.save()
                assert pixel==expected,("Desktop skipped own visible layered source",checks[-1])
            self.configure_view(0,mode="fullscreen",source="desktop",desktop_region=region,zoom=32,border=False,auto_refresh=False,monitor_id=self.monitor["id"],work_area=True)
            self.refreshed(0)
            self.native_view(0)
            new_b=(80,180,40)
            self.set_color(1,new_b)
            self.refreshed(0)
            image=self.exported_view(0,"layered-source-under-excluded-fullscreen-magnifier")
            expected=tuple((top*128+base*127+127)//255 for base,top in zip(a,new_b))+(255,)
            actual=image.getpixel((image.width//2,image.height//2))
            assert actual==expected,(expected,actual)
            self.evidence["checks"]["desktop-includes-own-opaque-and-alpha-layered-source-selectively-excludes-fullscreen-feedback"]={"region":region,"cases":checks,"feedback_expected":expected,"feedback_pixel":actual,"state":self.state()}
        finally:
            self.configure_view(0)
            user32.SetWindowLongPtrW(hwnd,-20,old_style)
            user32.SetWindowPos(hwnd,wintypes.HWND(-1),490,40,420,360,0x10)
            for user in (0,1):
                self.apply(user)

    def responsiveness_and_generation(self):
        self.anchored(0)
        large_frame=self.args.large_frame
        if large_frame:
            hwnd=self.report(0)["capture_hwnd"]
            assert user32.SetWindowPos(hwnd,None,0,0,4000,3990,0x10|0x2|0x4)
            bounds=Rect()
            assert user32.GetClientRect(hwnd,ctypes.byref(bounds))
            pixels=bounds.right*bounds.bottom
            assert 15_000_000<=pixels<=16_000_000,pixels
            # The child remains clipped by its unchanged small owned root, so
            # this real large client surface cannot cover B or the desktop.
            self.configure_view(0,mode="lens",width=64,height=64,auto_refresh=False)
            self.frame(0)
            image=self.exported_view(0,"large-owned-client-actual-first-frame")
            assert image.getpixel((32,32))==(*self.report(0)["capture_color"],255)
        else:
            self.set_delay(0,2500)
        trace_before=self.report(0)
        applied_before=trace_before["capture_delay_applied_count"]
        total_before=trace_before["capture_delay_total_ms"]
        configure_started=time.monotonic()
        if large_frame:
            self.magnifier("refresh",user=0)
        else:
            self.configure_view(0,mode="lens",width=64,height=64,auto_refresh=False)
        configure_elapsed=time.monotonic()-configure_started
        assert configure_elapsed<1.0,("Configure blocked on capture",configure_elapsed)
        active=eventually(lambda:self.magnifier()["users"][0],lambda u:u["in_flight"])
        latencies=[]
        count=self.report(1)["key_event_count"]
        for pressed in (True,False):
            started=time.monotonic()
            self.key(1,"f5",pressed)
            latencies.append(time.monotonic()-started)
        eventually(lambda:self.report(1),lambda r:r["key_event_count"]==count+2)
        assert max(latencies)<1.0,("input blocked on slow source capture",latencies)
        # Repeated Refresh commands coalesce into one pending intent, not a queue.
        for _ in range(3):
            self.magnifier("refresh",user=0)
        active_before_off=self.magnifier()["users"][0]
        assert active_before_off["in_flight"],"real capture job completed before generation-discard check"
        started=time.monotonic()
        self.configure_view(0)
        elapsed=time.monotonic()-started
        assert elapsed<1.0,("Off blocked on capture",elapsed)
        off_state=self.magnifier()["users"][0]
        assert off_state["in_flight"],"no outstanding real job remained at Off"
        sequence=off_state["capture_sequence"]
        time.sleep(3)
        state=self.magnifier()["users"][0]
        assert state["settings"]["mode"]=="off" and state["overlay"] is None and state["capture_sequence"]==sequence and not state["in_flight"]
        if not large_frame:
            self.set_delay(0,0)
        trace_after=self.report(0)
        if not large_frame:
            assert trace_after["capture_delay_applied_count"]==applied_before+1,("native slow capture was not applied exactly once",trace_before,trace_after)
            assert trace_after["capture_delay_total_ms"]==total_before+2500
            assert trace_after["last_print_hdc_non_null"]
            assert trace_after["wm_print_count"]+trace_after["wm_printclient_count"]>trace_before["wm_print_count"]+trace_before["wm_printclient_count"]
        self.evidence["checks"]["real-large-frame-job-responsive-coalesced-refresh-and-late-generation-discard" if large_frame else "slow-native-capture-does-not-block-input-coalesces-refresh-discards-late-generation"]={"scope":"real near16M native capture/copy/render job; redirected PrintWindow callback blocking not claimed" if large_frame else "native delayed print callback","source_pixels":pixels if large_frame else None,"configure_response_seconds":configure_elapsed,"key_response_seconds":latencies,"off_response_seconds":elapsed,"active_state":active,"active_before_off":active_before_off,"off_state":off_state,"late_state":state,"trace_before":trace_before,"trace_after":trace_after,"probes":self.reports()}

    def ink_and_lifecycle(self,eof=False):
        self.anchored(0)
        self.configure_view(0,mode="lens",width=64,height=64,border=False,auto_refresh=False)
        self.refreshed(0)
        self.configure(0,tool="brush",color=[255,0,0],alpha=128,width=8)
        self.key(1,"f5",True)
        eventually(lambda:self.report(1),lambda r:116 in r["held_keys"])
        x,y=self.points[0]
        self.stroke(0,(x-10,y),(x+10,y))
        self.go(0,(x,y))
        color=tuple(self.report(0)["capture_color"])
        assert self.exported_view(0,"manual-ink-not-yet-refreshed").getpixel((32,32))==(*color,255)
        self.refreshed(0)
        expected=tuple((s*128+b*127+127)//255 for s,b in zip((255,0,0),color))+(255,)
        assert self.exported_view(0,"explicit-rust-ink-in-magnified-frame").getpixel((32,32))==expected
        assert 116 in self.report(1)["held_keys"]
        self.input("pause")
        eventually(lambda:self.magnifier()["users"][0],lambda u:not u["overlay"]["visible"])
        self.client.tool("magnifier",{"operation":"refresh","user":0},error=True)
        self.input("resume")
        self.frame(0)
        self.configure(0)
        root=next(w for w in self.client.tool("routing_windows")["windows"] if w["pid"]==self.probes[0].pid and w["title"].startswith("ProtoInputMultiPlex Automation Test"))
        user32.ShowWindow(root["hwnd"],7)
        try:
            eventually(lambda:self.magnifier()["users"][0],lambda u:u["overlay"] and not u["overlay"]["visible"] and u["source_hwnd"] is None)
            self.client.tool("magnifier",{"operation":"export","user":0,"path":str(self.run/"minimized-stale.png")},error=True)
            assert self.state()["running"]
        finally:
            user32.ShowWindow(root["hwnd"],4)
        self.input("move_to",user=0,x=x,y=y)
        self.input("button",user=0,button="primary",pressed=True)
        self.input("button",user=0,button="primary",pressed=False)
        self.frame(0)
        self.probes[0].terminate()
        self.probes[0].wait(timeout=10)
        eventually(lambda:self.magnifier()["users"][0],lambda u:u["overlay"] and not u["overlay"]["visible"] and u["source_hwnd"] is None)
        assert self.state()["running"]
        self.configure_view(1,mode="lens",width=64,height=64,border=False,auto_refresh=False)
        self.frame(1)
        self.key(1,"f5",True)
        eventually(lambda:self.report(1),lambda r:116 in r["held_keys"])
        state=self.state()
        handles=[u["overlay"]["hwnd"] for u in state["users"]]+[u["overlay"]["hwnd"] for u in self.magnifier()["users"] if u["overlay"]]
        if state["annotation"]["overlay"]:
            handles.append(state["annotation"]["overlay"]["hwnd"])
        if eof:
            self.client.close()
            self.client=None
        else:
            self.client.tool("routing_stop")
        eventually(lambda:self.report(1),lambda r:not r["held_keys"] and not r["buttons"])
        eventually(lambda:[bool(user32.IsWindow(h)) for h in handles],lambda live:not any(live))
        self.evidence["checks"]["pause-minimize-source-close-stale-hide-live-routing-and-"+("eof" if eof else "stop")+"-cleanup"]={"before":state,"handles":handles,"after_probes":self.reports()}


def main():
    parser=argparse.ArgumentParser()
    parser.add_argument("--executable",type=Path,required=True)
    parser.add_argument("--x86-executable",type=Path,required=True)
    parser.add_argument("--runtime",type=Path,required=True)
    parser.add_argument("--eof-only",action="store_true")
    parser.add_argument("--lifecycle-only",action="store_true")
    parser.add_argument("--zorder-only",action="store_true")
    parser.add_argument("--responsiveness-only",action="store_true")
    parser.add_argument("--layered-only",action="store_true")
    parser.add_argument("--large-frame",action="store_true",help="Use a real near16M native/pixel job instead of an unexercised redirected print callback")
    args=parser.parse_args()
    gate=MagnifierGate(args)
    try:
        gate.start()
        gate.discover_view()
        if args.layered_only:
            gate.layered_source()
        elif args.responsiveness_only:
            gate.responsiveness_and_generation()
        elif args.zorder_only:
            gate.cursor_zorder()
        elif not (args.eof_only or args.lifecycle_only):
            gate.validation()
            gate.selected_pixels()
            gate.placement()
            gate.cursor_zorder()
            gate.dynamic_refresh()
            gate.fullscreen_cursor()
            gate.desktop_source()
            gate.layered_source()
            gate.responsiveness_and_generation()
        if not (args.zorder_only or args.responsiveness_only or args.layered_only):
            gate.ink_and_lifecycle(eof=args.eof_only)
        gate.evidence["result"]="PASS"
        print(f"PASS: independent magnifier native acceptance. Evidence: {gate.run}",flush=True)
    except Exception as error:
        gate.evidence["result"]="NOT ACCEPTED"
        gate.evidence["failure"]=repr(error)
        print(f"NOT ACCEPTED: {error!a}. Evidence: {gate.run}",flush=True)
        raise
    finally:
        gate.close()


if __name__=="__main__":
    main()
