//! Reversible forwarding of newly created, single-tab Explorer folder windows.
//! Existing windows and unconfirmed navigations are never closed.
use std::{collections::{HashMap,HashSet},path::{Path,PathBuf},sync::{Arc,Mutex,mpsc::{channel,Receiver,Sender},atomic::{AtomicBool,Ordering}},thread::{self,JoinHandle},time::{Duration,Instant}};
use anyhow::{ensure,Context,Result};
use windows::{core::{Interface,PCWSTR},Win32::{Foundation::{COLORREF,HWND,LPARAM,WPARAM},System::{Com::{CoCreateInstance,CoInitializeEx,CoUninitialize,CLSCTX_LOCAL_SERVER,COINIT_APARTMENTTHREADED},DataExchange::COPYDATASTRUCT,Variant::VARIANT},UI::{Accessibility::{SetWinEventHook,UnhookWinEvent,HWINEVENTHOOK},Shell::{IShellWindows,IWebBrowser2,IShellFolderViewDual,Folder2,ShellWindows},WindowsAndMessaging::*}}};
use windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled;

pub(crate) fn same_path(a:&Path,b:&Path)->bool{
    use std::os::windows::ffi::OsStrExt;
    let units=|p:&Path|p.as_os_str().encode_wide().map(|v|if v==47{92}else{v}).collect::<Vec<_>>();
    let mut a=units(a);let mut b=units(b);while a.len()>3&&a.last()==Some(&92){a.pop();}while b.len()>3&&b.last()==Some(&92){b.pop();}
    (unsafe{windows::Win32::Globalization::CompareStringOrdinal(&a,&b,true)})==windows::Win32::Globalization::CSTR_EQUAL
}
pub fn owns_integration(profile:&Path)->bool{
    use zsdesk_windows::integration::{self,Integration};
    std::path::absolute(profile).ok().zip(std::path::absolute(crate::storage::config_path()).ok()).is_some_and(|(a,b)|same_path(&a,&b))
        && integration::current()==Integration::Default
        && std::env::current_exe().is_ok_and(|exe|integration::win_e_enabled(&exe))
}
struct Apartment;
impl Drop for Apartment{fn drop(&mut self){unsafe{CoUninitialize();}}}
#[derive(Clone)]
struct Snapshot{window:HWND,pid:u32,folder:PathBuf,selected:Vec<PathBuf>,browser:IWebBrowser2,this_pc:bool,recycle:bool,
    /// Explorer is still navigating in this window. A `/select` launch first
    /// shows the shell's start location — which comes with its own default
    /// selection — and only then navigates to the requested folder, so that
    /// state must never be mistaken for the answer.
    busy:bool}
impl Snapshot{
    /// Navigation roots that are forwarded as a whole location instead of a
    /// file-system folder.
    fn special(&self)->bool{self.this_pc||self.recycle}
    /// Explorer shows its start location first and navigates afterwards, so an
    /// early snapshot can pair a folder with selected items that live somewhere
    /// else. Such a state must not be forwarded: it would open the wrong folder.
    fn consistent(&self)->bool{
        if self.special(){return self.selected.is_empty();}
        self.folder.is_absolute()&&self.selected.iter().all(|item|item.parent().is_some_and(|parent|same_path(parent,&self.folder)))
    }
    fn equivalent(&self,other:&Self)->bool{self.window==other.window&&self.pid==other.pid&&self.this_pc==other.this_pc&&self.recycle==other.recycle&&same_path(&self.folder,&other.folder)
        && self.selected.len()==other.selected.len()&&self.selected.iter().all(|a|other.selected.iter().any(|b|same_path(a,b)))}
    /// Reads the window again through its own browser object. Enumerating
    /// `IShellWindows` can report a window twice while Explorer navigates, and
    /// that duplicate used to make a settled window look like a changed one.
    fn refresh(&self)->Option<Self>{
        let view=unsafe{self.browser.Document().ok()?}.cast::<IShellFolderViewDual>().ok()?;
        let folder=unsafe{view.Folder().ok()?}.cast::<Folder2>().ok()?;
        let folder=PathBuf::from(unsafe{folder.Self_().ok()?.Path().ok()?}.to_string());
        let items=unsafe{view.SelectedItems().ok()?};let count=unsafe{items.Count().ok()?};
        if !(0..=128).contains(&count){return None;}
        let mut selected=Vec::with_capacity(count as usize);
        for i in 0..count{selected.push(PathBuf::from(unsafe{items.Item(&VARIANT::from(i)).ok()?.Path().ok()?}.to_string()));}
        Some(Self{window:self.window,pid:self.pid,folder,selected,browser:self.browser.clone(),this_pc:self.this_pc,recycle:self.recycle,
            busy:busy(&self.browser)})
    }
}
/// Whether the window still has a navigation in flight.
fn busy(browser:&IWebBrowser2)->bool{unsafe{browser.Busy()}.map(|value|value.as_bool()).unwrap_or(false)}
fn windows(shell:&IShellWindows)->Result<Vec<Snapshot>>{
    let mut result=Vec::new();let mut counts=HashMap::new();
    for index in 0..unsafe{shell.Count()?}.clamp(0,128){
        let Ok(dispatch)=(unsafe{shell.Item(&VARIANT::from(index))})else{continue;};
        let Ok(browser)=dispatch.cast::<IWebBrowser2>()else{continue;};
        let Ok(handle)=(unsafe{browser.HWND()})else{continue;};let window=HWND(handle.0 as*mut _);
        *counts.entry(window.0 as isize).or_insert(0usize)+=1;
        let mut class=[0u16;80];let n=unsafe{GetClassNameW(window,&mut class)}.max(0)as usize;
        if !matches!(String::from_utf16_lossy(&class[..n]).as_str(),"CabinetWClass"|"ExploreWClass"){continue;}
        let expected=PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(||r"C:\Windows".into())).join("explorer.exe");
        if !unsafe{browser.FullName()}.is_ok_and(|name|same_path(Path::new(&name.to_string()),&expected)){continue;}
        let mut pid=0;unsafe{GetWindowThreadProcessId(window,Some(&mut pid));}if pid==0{continue;}
        let snapshot=(||->windows::core::Result<Snapshot>{
            let view=unsafe{browser.Document()?}.cast::<IShellFolderViewDual>()?;
            let folder=unsafe{view.Folder()?}.cast::<Folder2>()?;
            let folder=PathBuf::from(unsafe{folder.Self_()?.Path()?}.to_string());
            let items=unsafe{view.SelectedItems()?};let count=unsafe{items.Count()?};
            if !(0..=128).contains(&count){return Err(windows::core::Error::from_hresult(windows::core::HRESULT(0x80070057u32 as i32)));}
            let mut selected=Vec::with_capacity(count as usize);
            for i in 0..count{selected.push(PathBuf::from(unsafe{items.Item(&VARIANT::from(i))?.Path()?}.to_string()));}
            let this_pc=folder.to_string_lossy().eq_ignore_ascii_case("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}");
            let recycle=folder.to_string_lossy().eq_ignore_ascii_case(zsdesk_core::model::explorer::RECYCLE_BIN_ADDRESS);
            Ok(Snapshot{window,pid,folder,selected,browser:browser.clone(),this_pc,recycle,busy:busy(&browser)})
        })();
        // A window mid-navigation may pair a start folder with unrelated
        // selected items. Keep it and let the forwarding loop wait until it
        // settles instead of dropping the window and handing it back to Explorer.
        if let Ok(mut snapshot)=snapshot{
            // Explorer sometimes leaves the start folder in place while the
            // requested item is already selected (the `/select` hand-off).
            // The item's own folder is the location the caller asked for, so
            // forward that instead of opening an unrelated folder.
            if !snapshot.special()&&!snapshot.consistent()&&!snapshot.selected.is_empty(){
                let parent=snapshot.selected[0].parent().map(Path::to_path_buf);
                if let Some(parent)=parent{
                    if snapshot.selected.iter().all(|item|item.parent().is_some_and(|path|same_path(path,&parent))){snapshot.folder=parent;}
                }
            }
            if snapshot.special()||snapshot.folder.is_absolute(){result.push(snapshot);}
        }
    }
    result.retain(|snapshot|counts.get(&(snapshot.window.0 as isize))==Some(&1));Ok(result)
}
fn profile_windows(profile:&Path)->Vec<HWND>{
    let property=crate::instance::property(profile);let mut windows=Vec::new();let mut previous=None;
    while let Ok(hwnd)=unsafe{FindWindowExW(None,previous,windows::core::w!("ZSDesk.Native.Explorer"),None)}{
        previous=Some(hwnd);if unsafe{GetPropW(hwnd,PCWSTR(property.as_ptr()))}.0 as usize==1{windows.push(hwnd);}
    }windows
}
fn find_target(profile:&Path)->Option<HWND>{profile_windows(profile).into_iter().next()}
/// Appends one line of evidence to `forwarding.log`: which window state a
/// takeover was based on and which selection followed it. When a caller's
/// "show this item" request lands in the wrong place, this is what shows what
/// Windows actually reported. The file is discarded once it grows past 128 KiB.
fn log(profile:&Path,value:serde_json::Value){
    let path=profile.with_file_name("forwarding.log");
    if std::fs::metadata(&path).is_ok_and(|meta|meta.len()>128*1024){let _=std::fs::remove_file(&path);}
    let line=format!("{}\n",serde_json::to_string(&value).unwrap_or_default());
    if let Ok(mut file)=std::fs::OpenOptions::new().create(true).append(true).open(&path){
        use std::io::Write;let _=file.write_all(line.as_bytes());
    }
}
fn send(window:HWND,tag:usize,packet:&[u8])->bool{
    let data=COPYDATASTRUCT{dwData:tag,cbData:packet.len()as u32,lpData:packet.as_ptr()as*mut _};let mut response=0;
    unsafe{SendMessageTimeoutW(window,WM_COPYDATA,WPARAM(0),LPARAM(&data as*const _ as isize),SMTO_ABORTIFHUNG,500,Some(&mut response))}.0!=0&&response==1
}
fn forward(profile:&Path,executable:&Path,snapshot:&Snapshot)->Result<()>{
    use zsdesk_core::model::explorer::ShellLocation;
    log(profile,serde_json::json!({"event":"forward","window":snapshot.window.0 as isize,"pid":snapshot.pid,"busy":snapshot.busy,
        "folder":snapshot.folder,"selected":snapshot.selected,"this_pc":snapshot.this_pc,"recycle":snapshot.recycle}));
    let packet=if snapshot.this_pc{serde_json::to_vec(&(Some(ShellLocation::ThisPc),None::<String>))?}
        else if snapshot.recycle{serde_json::to_vec(&(Some(ShellLocation::recycle_bin()),None::<String>))?}
        else{crate::reveal::encode(&snapshot.folder,&snapshot.selected).context("文件位置请求超出支持范围")?};
    if find_target(profile).is_none(){
        // The application host is already running: hand the folder over with one
        // packet instead of starting a launcher process that would only forward
        // the same arguments to it. That process costs a quarter second.
        let mut arguments:Vec<std::ffi::OsString>=vec!["--profile-dir".into(),profile.parent().context("配置目录缺失")?.as_os_str().to_owned()];
        if snapshot.this_pc{arguments.push("--this-pc".into());}
        else if snapshot.recycle{arguments.push("--recycle-bin".into());}
        else{arguments.push(snapshot.folder.as_os_str().to_owned());}
        if !crate::runtime::enqueue_host(profile,arguments)?{
            use std::os::windows::process::CommandExt;
            let mut command=std::process::Command::new(executable);
            command.arg("--profile-dir").arg(profile.parent().context("配置目录缺失")?);
            if snapshot.this_pc{command.arg("--this-pc");}else if snapshot.recycle{command.arg("--recycle-bin");}else{command.arg(&snapshot.folder);}
            command
                .creation_flags(0x08000000).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn()?;
        }
    }
    let deadline=Instant::now()+Duration::from_secs(8);
    let target=loop{if let Some(target)=find_target(profile){break target;}ensure!(Instant::now()<deadline,"文件管理窗口未就绪");thread::sleep(Duration::from_millis(10));};
    ensure!(send(target,if snapshot.special(){crate::instance::OPEN_MAGIC}else{crate::reveal::COPYDATA_TAG},&packet),"目标窗口未接受文件位置请求");
    let verify=if snapshot.this_pc{crate::reveal::VERIFY_THIS_PC}
        else if snapshot.recycle{crate::reveal::VERIFY_RECYCLE_BIN}
        else{crate::reveal::VERIFY_TAG};
    let confirmation:&[u8]=if snapshot.special(){&[]}else{&packet};
    // A window hosts one tab, so the request may be answered by a window that
    // was created for it rather than by the window that received it.
    let deadline=Instant::now()+Duration::from_secs(4);
    let target=loop{
        if let Some(confirmed)=profile_windows(profile).into_iter().find(|window|send(*window,verify,confirmation)){break confirmed;}
        ensure!(Instant::now()<deadline,"导航或选中状态未确认，保留系统窗口");thread::sleep(Duration::from_millis(15));
    };
    // The window exists before the requested selection does. A `/select` launch
    // navigates and then selects, and a caller of `SHOpenFolderAndSelectItems`
    // lets the shell select into the window it just asked for, which arrives a
    // moment later still. The folder is forwarded first so it opens at once and
    // the selection follows as a second request — the system window is closed
    // right after this, so a selection that shows up later would otherwise be
    // lost and the item would never be highlighted. This window is invisible
    // the whole time, so waiting for the selection costs the user nothing.
    if !snapshot.special()&&snapshot.selected.is_empty(){
        let deadline=Instant::now()+FOLLOW_UP_WAIT;
        while Instant::now()<deadline{
            thread::sleep(Duration::from_millis(40));
            let Some(current)=snapshot.refresh()else{break;};
            if current.busy||current.selected.is_empty()||!current.consistent(){continue;}
            if let Some(follow_up)=crate::reveal::encode(&current.folder,&current.selected){
                log(profile,serde_json::json!({"event":"follow_up","window":snapshot.window.0 as isize,"pid":snapshot.pid,
                    "folder":current.folder,"selected":current.selected}));
                let _=send(target,crate::reveal::COPYDATA_TAG,&follow_up);
            }
            break;
        }
    }
    // A fresh read prevents closing a window the user has already navigated
    // elsewhere, turned into multiple tabs, or placed behind a modal dialog.
    // A concealed window keeps navigating after it is hidden, so a folder that
    // moved on its own is not user intent: only a visible window must still
    // match the state that was forwarded.
    let hidden=!unsafe{IsWindowVisible(snapshot.window)}.as_bool();
    let mut settled=None;
    for _ in 0..6{
        if let Some(latest)=snapshot.refresh(){settled=Some(latest);break;}
        thread::sleep(Duration::from_millis(60));
    }
    let latest=settled.context("来源窗口状态已改变")?;
    ensure!(unsafe{IsWindowEnabled(snapshot.window)}.as_bool(),"来源窗口已改变，保留系统窗口");
    ensure!(hidden||snapshot.equivalent(&latest),"来源窗口已改变，保留系统窗口");
    let foreground=unsafe{GetForegroundWindow()}==snapshot.window;
    if foreground{crate::instance::request_activation(target);}
    unsafe{snapshot.browser.Quit()}?;
    // A `/select` launch runs in its own explorer.exe helper process. Once its
    // only window is taken over that helper has nothing left to show, but it
    // does not always exit; Windows then hands the next `/select` to it, and a
    // helper without a window answers by opening its own default folder — which
    // is how a reveal ends up highlighting an unrelated item or nothing at all.
    reap_helper(profile,snapshot.pid);
    Ok(())
}
/// Ends an explorer.exe helper that owned a taken-over window and kept running
/// without any window of its own. The shell process and any process that still
/// shows a window are left alone.
fn reap_helper(profile:&Path,pid:u32){
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess,TerminateProcess,PROCESS_TERMINATE};
    if pid==0{return;}
    let mut shell=0u32;unsafe{GetWindowThreadProcessId(GetShellWindow(),Some(&mut shell));}
    if pid==shell{return;}
    for _ in 0..20{if !owns_windows(pid){break;}thread::sleep(Duration::from_millis(20));}
    if owns_windows(pid){return;}
    unsafe{
        if let Ok(process)=OpenProcess(PROCESS_TERMINATE,false,pid){
            let _=TerminateProcess(process,0);let _=CloseHandle(process);
            log(profile,serde_json::json!({"event":"reap","pid":pid}));
        }
    }
}
unsafe extern "system" fn note_window_of(window:HWND,param:LPARAM)->windows::core::BOOL{
    let wanted=param.0 as *const (u32,*mut bool);
    let mut pid=0u32;unsafe{GetWindowThreadProcessId(window,Some(&mut pid));}
    if !wanted.is_null()&&pid==unsafe{(*wanted).0}{unsafe{*(*wanted).1=true;}return windows::core::BOOL(0);}
    windows::core::BOOL(1)
}
fn owns_windows(pid:u32)->bool{
    let mut found=false;let context=(pid,&mut found as *mut bool);
    unsafe{let _=EnumWindows(Some(note_window_of),LPARAM(&context as *const _ as isize));}
    found
}

/// Explorer opens a double-clicked file-system folder itself instead of
/// resolving the `Folder\shell\open` registration, so the first moment a
/// takeover is possible is the `CabinetWClass` window it creates. A shell
/// window event reports that window while it is still being shown; hiding it
/// immediately keeps the system window from painting at all.
static WINDOW_EVENTS: Mutex<Option<Sender<isize>>> = Mutex::new(None);
/// Windows handed back to Explorer. Showing one raises another show event,
/// which must not be concealed again or the window would never come back.
static RELEASED: Mutex<Vec<isize>> = Mutex::new(Vec::new());
fn released(window:isize)->bool{RELEASED.lock().is_ok_and(|list|list.contains(&window))}
fn release(window:isize){if let Ok(mut list)=RELEASED.lock(){if !list.contains(&window){if list.len()>=256{list.remove(0);}list.push(window);}}}
/// Concealed windows that were also turned into tool windows. Explorer asks the
/// shell for a taskbar button as soon as its window is shown, so the window is
/// created without one; a window handed back must get the button again.
static TOOL_WINDOWS: Mutex<Vec<isize>> = Mutex::new(Vec::new());
fn mark_tool(window:isize){if let Ok(mut list)=TOOL_WINDOWS.lock(){if !list.contains(&window){if list.len()>=256{list.remove(0);}list.push(window);}}}
fn unmark_tool(window:isize)->bool{
    TOOL_WINDOWS.lock().map(|mut list|list.iter().position(|item|*item==window).map(|index|{list.remove(index);}).is_some()).unwrap_or(false)
}
/// Set while the takeover is armed. The hook thread hides a new Explorer
/// window itself: waiting for the forwarding thread would leave the window on
/// screen for a couple of frames.
static CONCEAL: AtomicBool = AtomicBool::new(false);
/// Deadline before which new Explorer windows are left alone. ZSDesk itself
/// falls back to Explorer when it cannot show a folder; concealing that window
/// and forwarding it back would loop.
static SUPPRESS_UNTIL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How long a forwarded window is kept around to let a late selection land.
/// `SHOpenFolderAndSelectItems` (used by browsers' "show in folder") asks the
/// shell to select into a window that already exists, so its selection can
/// arrive well after the folder does.
const FOLLOW_UP_WAIT:Duration=Duration::from_millis(1500);

/// Leaves new Explorer windows alone for `window` (used by these fallbacks).
pub fn suppress(window:Duration){
    let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|value|value.as_millis() as u64).unwrap_or(0);
    SUPPRESS_UNTIL.fetch_max(now+window.as_millis() as u64,Ordering::Relaxed);
}
fn suppressed()->bool{
    let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|value|value.as_millis() as u64).unwrap_or(0);
    now<SUPPRESS_UNTIL.load(Ordering::Relaxed)
}

unsafe extern "system" fn explorer_window_event(_hook:HWINEVENTHOOK,event:u32,window:HWND,object:i32,child:i32,_thread:u32,_time:u32){
    // OBJID_WINDOW is 0; child 0 means the window itself, not an item in it.
    if (event!=EVENT_OBJECT_SHOW&&event!=EVENT_OBJECT_CREATE)||object!=0||child!=0||window.0.is_null(){return;}
    let mut class=[0u16;24];
    let length=unsafe{GetClassNameW(window,&mut class)}.max(0)as usize;
    if !matches!(String::from_utf16_lossy(&class[..length]).as_str(),"CabinetWClass"|"ExploreWClass"){return;}
    if released(window.0 as isize)||suppressed(){return;}
    if CONCEAL.load(Ordering::Relaxed){
        // Hiding is queued behind Explorer's own window creation and lands a
        // couple of frames late, so the window is made fully transparent while
        // it is still being created: that lands before the first paint. The same
        // event drops the taskbar button, which the shell would otherwise create
        // and destroy within a few frames.
        if event==EVENT_OBJECT_CREATE{
            let ex=unsafe{GetWindowLongPtrW(window,GWL_EXSTYLE)};
            unsafe{SetWindowLongPtrW(window,GWL_EXSTYLE,ex|WS_EX_LAYERED.0 as isize|WS_EX_TOOLWINDOW.0 as isize)};
            unsafe{let _=SetLayeredWindowAttributes(window,COLORREF(0),0,LWA_ALPHA);}
            mark_tool(window.0 as isize);
        }
        unsafe{let _=ShowWindow(window,SW_HIDE);}
    }
    if let Ok(cell)=WINDOW_EVENTS.lock(){if let Some(sink)=cell.as_ref(){let _=sink.send(window.0 as isize);}}
}
struct WindowEvents{worker:Option<JoinHandle<()>>}
impl WindowEvents{
    fn start(stop:&Arc<AtomicBool>)->(Self,Receiver<isize>){
        let (sink,events)=channel::<isize>();
        if let Ok(mut cell)=WINDOW_EVENTS.lock(){*cell=Some(sink);}
        let signal=stop.clone();
        let worker=thread::Builder::new().name("zsdesk-explorer-window-events".into()).stack_size(256*1024).spawn(move||{
            // Out-of-context hooks are delivered by this thread's message loop.
            let hook=unsafe{SetWinEventHook(EVENT_OBJECT_CREATE,EVENT_OBJECT_SHOW,None,Some(explorer_window_event),0,0,WINEVENT_OUTOFCONTEXT|WINEVENT_SKIPOWNPROCESS)};
            if hook.is_invalid(){return;}
            let mut message=MSG::default();
            while !signal.load(Ordering::Acquire){
                while unsafe{PeekMessageW(&mut message,None,0,0,PM_REMOVE)}.as_bool(){
                    unsafe{let _=TranslateMessage(&message);DispatchMessageW(&message);}
                }
                thread::sleep(Duration::from_millis(4));
            }
            unsafe{let _=UnhookWinEvent(hook);}
        }).ok();
        (Self{worker},events)
    }
}
impl Drop for WindowEvents{fn drop(&mut self){CONCEAL.store(false,Ordering::Relaxed);if let Ok(mut cell)=WINDOW_EVENTS.lock(){*cell=None;}if let Some(worker)=self.worker.take(){let _=worker.join();}}}
pub struct Bridge{stop:Arc<AtomicBool>,worker:Option<JoinHandle<()>>,events:Option<WindowEvents>}
impl Bridge{
    pub fn start(profile:&Path)->Result<Option<Self>>{
        if !same_path(&std::path::absolute(profile)?,&std::path::absolute(crate::storage::config_path())?){return Ok(None);}
        let profile=profile.to_path_buf();let stop=Arc::new(AtomicBool::new(false));let signal=stop.clone();
        let (events,hook_events)=WindowEvents::start(&stop);
        let worker=thread::Builder::new().name("zsdesk-explorer-forwarding".into()).stack_size(512*1024).spawn(move||{
            if unsafe{CoInitializeEx(None,COINIT_APARTMENTTHREADED)}.is_err(){return;}let _apartment=Apartment;
            let mut shell:Option<IShellWindows>=None;let mut baseline=HashSet::new();let mut candidates:HashMap<isize,(Snapshot,Instant)>=HashMap::new();
            let mut concealed:HashSet<isize>=HashSet::new();let mut since:HashMap<isize,Instant>=HashMap::new();
            let mut active=false;let mut recycle_allowed=true;let mut refresh=Instant::now()-Duration::from_secs(3);let mut scan=Instant::now();let mut forwarded=0u64;
            let reveal=|window:isize|{let hwnd=HWND(window as*mut _);if unsafe{IsWindow(Some(hwnd))}.as_bool(){release(window);
                // Undo the tool-window bit so a window Explorer keeps gets its
                // taskbar button and Alt+Tab entry back.
                if unmark_tool(window){let ex=unsafe{GetWindowLongPtrW(hwnd,GWL_EXSTYLE)};unsafe{SetWindowLongPtrW(hwnd,GWL_EXSTYLE,ex&!(WS_EX_TOOLWINDOW.0 as isize));}}
                unsafe{let _=SetLayeredWindowAttributes(hwnd,COLORREF(0),255,LWA_ALPHA);let _=ShowWindow(hwnd,SW_SHOW);}}};
            while !signal.load(Ordering::Acquire){
                // Hide a newly shown Explorer window before it paints, then let
                // the regular forwarding below move its location into ZSDesk.
                while let Ok(raw)=hook_events.try_recv(){
                    let window=HWND(raw as*mut _);
                    // The hook has already hidden the window when it is armed,
                    // so an invisible window here is still a takeover candidate.
                    if !active||baseline.contains(&raw)||concealed.contains(&raw)||!unsafe{IsWindow(Some(window))}.as_bool(){continue;}
                    if unsafe{IsWindowVisible(window)}.as_bool(){unsafe{let _=ShowWindow(window,SW_HIDE);}}
                    concealed.insert(raw);since.entry(raw).or_insert(Instant::now());
                }
                if !active{
                    for window in concealed.drain(){reveal(window);}
                    since.clear();
                }else{
                    // A window that could not be taken over must not stay invisible.
                    let stale:Vec<_>=since.iter().filter(|(_,at)|at.elapsed()>=Duration::from_millis(4000)).map(|(window,_)|*window).collect();
                    for window in stale{concealed.remove(&window);since.remove(&window);reveal(window);}
                }
                if refresh.elapsed()>=Duration::from_secs(2){
                    let session=crate::storage::load(&profile).ok();
                    let enabled=owns_integration(&profile)&&session.as_ref().is_some_and(|session|session.ui.forward_unhandled_windows);
                    recycle_allowed=session.as_ref().is_none_or(|session|session.ui.recycle_bin_takeover);
                    if enabled!=active{active=enabled;CONCEAL.store(enabled,Ordering::Relaxed);shell=None;baseline.clear();candidates.clear();write_status(&profile,active,forwarded,None);}
                    refresh=Instant::now();
                }
                if active && scan.elapsed()<Duration::from_millis(30){thread::sleep(Duration::from_millis(6));continue;}
                scan=Instant::now();
                if active{
                    if shell.is_none(){
                        if let Ok(value)=unsafe{CoCreateInstance::<_,IShellWindows>(&ShellWindows,None,CLSCTX_LOCAL_SERVER)}{
                            if let Ok(items)=windows(&value){baseline.extend(items.iter().map(|item|item.window.0 as isize));}
                            shell=Some(value);
                        }
                    }else if let Some(shell)=&shell{
                        if let Ok(items)=windows(shell){
                            let present:HashSet<_>=items.iter().map(|item|item.window.0 as isize).collect();
                            baseline.retain(|window|unsafe{IsWindow(Some(HWND(*window as*mut _)))}.as_bool());candidates.retain(|window,_|present.contains(window));
                            // A concealed window the shell does not report as a
                            // folder location is not ours to take over (Control
                            // Panel, search results): give it back quickly.
                            let unknown:Vec<_>=concealed.iter().copied().filter(|window|!present.contains(window)).collect();
                            for window in unknown{if since.get(&window).is_none_or(|at|at.elapsed()>=Duration::from_millis(700)){concealed.remove(&window);since.remove(&window);reveal(window);}}
                            concealed.retain(|window|unsafe{IsWindow(Some(HWND(*window as*mut _)))}.as_bool());
                            for snapshot in items{
                                let key=snapshot.window.0 as isize;
                                let hidden=concealed.contains(&key);
                                if baseline.contains(&key)||(!hidden&&!unsafe{IsWindowVisible(snapshot.window)}.as_bool())||!unsafe{IsWindowEnabled(snapshot.window)}.as_bool(){continue;}
                                if snapshot.recycle&&!recycle_allowed{continue;}
                                // Explorer resolves `explorer.exe /select` in two
                                // steps: a window is created at its start
                                // location and only then navigates to the
                                // requested one. Forwarding the first state would
                                // open the wrong folder, and that start location
                                // brings its own default selection along, which
                                // used to be forwarded as if it were the answer.
                                if snapshot.busy||!snapshot.consistent(){candidates.remove(&key);continue;}
                                // A concealed window is invisible, so a short
                                // settle costs nothing while it still keeps a
                                // start state that has not begun navigating from
                                // being taken for the request.
                                let settle=if hidden{Duration::from_millis(250)}else{Duration::from_millis(600)};
                                match candidates.get(&key){
                                    Some((old,at)) if old.equivalent(&snapshot)&&at.elapsed()>=settle=>{
                                        baseline.insert(key);candidates.remove(&key);concealed.remove(&key);since.remove(&key);
                                        let result=std::env::current_exe().map_err(anyhow::Error::from).and_then(|exe|forward(&profile,&exe,&snapshot));
                                        match result{Ok(())=>{forwarded+=1;write_status(&profile,true,forwarded,None);},Err(error)=>{if hidden{reveal(key);}write_status(&profile,true,forwarded,Some(&format!("{error:#}"))) }}
                                    }
                                    Some((old,_)) if old.equivalent(&snapshot)=>{}
                                    _=>{
                                        log(&profile,serde_json::json!({"event":"candidate","window":key,"pid":snapshot.pid,"hidden":hidden,
                                            "busy":snapshot.busy,"folder":snapshot.folder,"selected":snapshot.selected}));
                                        candidates.insert(key,(snapshot,Instant::now()));
                                    }
                                }
                            }
                        }
                    }
                }
                thread::sleep(Duration::from_millis(6));
            }
        })?;
        Ok(Some(Self{stop,worker:Some(worker),events:Some(events)}))
    }
}
impl Drop for Bridge{fn drop(&mut self){self.stop.store(true,Ordering::Release);if let Some(events)=self.events.take(){drop(events);}if self.worker.as_ref().is_some_and(JoinHandle::is_finished){if let Some(worker)=self.worker.take(){let _=worker.join();}}}}
fn write_status(profile:&Path,active:bool,forwarded:u64,error:Option<&str>){
    if let Ok(bytes)=serde_json::to_vec(&serde_json::json!({"schema":1,"pid":std::process::id(),"active":active,"forwarded":forwarded,"last_error":error})){
        let _=crate::quick_search::control::atomic_write(&profile.with_file_name("explorer-forwarding-status.json"),&bytes);
    }
}

#[cfg(test)] mod tests{
    use super::*;
    #[test]fn path_comparison_preserves_unicode_and_normalizes_windows_spelling(){assert!(same_path(Path::new(r"E:\资料\Äbc"),Path::new("e:/资料/äBC/")));assert!(!same_path(Path::new(r"E:\a"),Path::new(r"E:\ab")));}
    #[test]
    #[ignore="Explicitly selected owned Explorer window and test profile required"]
    fn native_forwarding_confirms_navigation_before_closing_source()->Result<()>{
        let hwnd:usize=std::env::var("ZSDESK_FORWARD_TEST_HWND")?.parse()?;
        let profile=PathBuf::from(std::env::var_os("ZSDESK_FORWARD_TEST_PROFILE").context("profile")?);
        let executable=PathBuf::from(std::env::var_os("ZSDESK_FORWARD_TEST_EXE").context("exe")?);
        let output=PathBuf::from(std::env::var_os("ZSDESK_NATIVE_TEST_OUTPUT").context("output")?);
        unsafe{CoInitializeEx(None,COINIT_APARTMENTTHREADED)}.ok()?;let _apartment=Apartment;
        let shell:IShellWindows=unsafe{CoCreateInstance(&ShellWindows,None,CLSCTX_LOCAL_SERVER)}?;
        let snapshot=windows(&shell)?.into_iter().find(|item|item.window.0 as usize==hwnd).context("test Explorer window")?;
        ensure!(snapshot.folder.starts_with(&output),"test must be inside isolated fixtures");
        let selection=snapshot.selected.clone();forward(&profile,&executable,&snapshot)?;
        let deadline=Instant::now()+Duration::from_secs(3);while unsafe{IsWindow(Some(snapshot.window))}.as_bool(){ensure!(Instant::now()<deadline,"source window remained open");thread::sleep(Duration::from_millis(30));}
        let value=serde_json::json!({"state":"passed","folder":snapshot.folder,"selected":selection,"navigation_confirmed":true,"source_closed":true});
        std::fs::write(output.join("forwarding.current.json"),serde_json::to_vec_pretty(&value)?)?;println!("{value}");Ok(())
    }
}
