//! Real cross-process X11 DND coverage. XTest is dynamically loaded so these
//! tests do not add a build-time dependency to the application or other targets.
use super::*;
use std::ffi::{c_char, c_int, c_uint, c_ulong, c_void, CString};
use std::time::{Duration, Instant};

fn pump_for(duration: Duration) {
    let end = Instant::now() + duration;
    while Instant::now() < end {
        while glib::MainContext::default().pending() {
            glib::MainContext::default().iteration(false);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

struct X11 {
    libraries: [*mut c_void; 2],
    display: *mut c_void,
}

struct TestChild(std::process::Child);
impl Drop for TestChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
extern "C" {
    fn dlopen(name: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(library: *mut c_void, name: *const c_char) -> *mut c_void;
    fn dlclose(library: *mut c_void) -> c_int;
}
impl X11 {
    unsafe fn symbol<T: Copy>(&self, name: &str) -> T {
        let name = CString::new(name).unwrap();
        for library in self.libraries {
            let pointer = dlsym(library, name.as_ptr());
            if !pointer.is_null() {
                return std::mem::transmute_copy(&pointer);
            }
        }
        panic!("Missing X11 test symbol {name:?}")
    }
    fn new() -> Self {
        unsafe {
            let libraries = [
                dlopen(b"libX11.so.6\0".as_ptr().cast(), 2),
                dlopen(b"libXtst.so.6\0".as_ptr().cast(), 2),
            ];
            assert!(
                libraries.iter().all(|p| !p.is_null()),
                "Xvfb DND gate needs libX11 and libXtst"
            );
            let mut x = Self {
                libraries,
                display: std::ptr::null_mut(),
            };
            let open: unsafe extern "C" fn(*const c_char) -> *mut c_void = x.symbol("XOpenDisplay");
            x.display = open(std::ptr::null());
            assert!(
                !x.display.is_null(),
                "Run the external DND gate under real Xvfb"
            );
            x
        }
    }
    fn windows_named(&self, title: &str) -> Vec<c_ulong> {
        unsafe {
            let root: unsafe extern "C" fn(*mut c_void) -> c_ulong =
                self.symbol("XDefaultRootWindow");
            let query: unsafe extern "C" fn(
                *mut c_void,
                c_ulong,
                *mut c_ulong,
                *mut c_ulong,
                *mut *mut c_ulong,
                *mut c_uint,
            ) -> c_int = self.symbol("XQueryTree");
            let fetch: unsafe extern "C" fn(*mut c_void, c_ulong, *mut *mut c_char) -> c_int =
                self.symbol("XFetchName");
            let free: unsafe extern "C" fn(*mut c_void) -> c_int = self.symbol("XFree");
            let (mut r, mut parent, mut count) = (0, 0, 0);
            let mut children = std::ptr::null_mut();
            query(
                self.display,
                root(self.display),
                &mut r,
                &mut parent,
                &mut children,
                &mut count,
            );
            let mut found = Vec::new();
            for i in 0..count as usize {
                let window = *children.add(i);
                let mut name = std::ptr::null_mut();
                if fetch(self.display, window, &mut name) != 0 && !name.is_null() {
                    if std::ffi::CStr::from_ptr(name).to_string_lossy() == title {
                        found.push(window);
                    }
                    free(name.cast());
                }
            }
            if !children.is_null() {
                free(children.cast());
            }
            found
        }
    }
    fn window(&self, title: &str) -> Option<c_ulong> {
        let windows = self.windows_named(title);
        assert!(
            windows.len() <= 1,
            "X11 root scan found multiple toplevels titled {title:?}: {windows:?}; window selection is ambiguous"
        );
        windows.into_iter().next()
    }
    fn place(&self, window: c_ulong, x: i32) {
        unsafe {
            let place: unsafe extern "C" fn(
                *mut c_void,
                c_ulong,
                c_int,
                c_int,
                c_uint,
                c_uint,
            ) -> c_int = self.symbol("XMoveResizeWindow");
            place(self.display, window, x, 100, 300, 300);
        }
        self.flush();
    }
    fn flush(&self) {
        unsafe {
            let flush: unsafe extern "C" fn(*mut c_void) -> c_int = self.symbol("XFlush");
            flush(self.display);
        }
        pump_for(Duration::from_millis(80));
    }
    fn motion(&self, x: i32, y: i32) {
        unsafe {
            let motion: unsafe extern "C" fn(*mut c_void, c_int, c_int, c_int, c_ulong) -> c_int =
                self.symbol("XTestFakeMotionEvent");
            motion(self.display, -1, x, y, 0);
        }
        self.flush();
    }
    fn button(&self, down: bool) {
        unsafe {
            let button: unsafe extern "C" fn(*mut c_void, c_uint, c_int, c_ulong) -> c_int =
                self.symbol("XTestFakeButtonEvent");
            button(self.display, 1, down.into(), 0);
        }
        self.flush();
    }
}
impl Drop for X11 {
    fn drop(&mut self) {
        unsafe {
            let close: unsafe extern "C" fn(*mut c_void) -> c_int = self.symbol("XCloseDisplay");
            close(self.display);
            for library in self.libraries {
                dlclose(library);
            }
        }
    }
}

fn external_source(root: &Path) {
    let title = std::env::var("TASK8_EXTERNAL_DND_SOURCE_TITLE")
        .expect("parent must provide the external X11 source title");
    let window = gtk::Window::builder()
        .title(&title)
        .default_width(300)
        .default_height(300)
        .build();
    let label = gtk::Label::new(Some("External MOVE source"));
    window.set_child(Some(&label));
    let source = gtk::DragSource::new();
    source.set_actions(gtk::gdk::DragAction::MOVE);
    let mut text = format!(
        "{}\r\n",
        Location::from_local_path(&root.join("source.txt")).uri()
    );
    let mode = std::env::var("TASK8_EXTERNAL_DND_MODE").unwrap_or_default();
    if mode.starts_with("partial") {
        let missing = format!(
            "{}\r\n",
            Location::from_local_path(&root.join("missing.txt")).uri()
        );
        if mode == "partial_then_success" {
            text.insert_str(0, &missing);
        } else {
            text.push_str(&missing);
        }
    }
    source.set_content(Some(&gtk::gdk::ContentProvider::for_bytes(
        "text/uri-list",
        &glib::Bytes::from_owned(text.into_bytes()),
    )));
    let done = Rc::new(std::cell::Cell::new(false));
    let finished = done.clone();
    let root = root.to_path_buf();
    source.connect_drag_end(move |_, _, delete| {
        if delete {
            std::fs::remove_file(root.join("source.txt")).unwrap();
        }
        std::fs::write(
            root.join("finished"),
            if delete { "move" } else { "rejected" },
        )
        .unwrap();
        finished.set(true);
    });
    label.add_controller(source);
    window.present();
    let end = Instant::now() + Duration::from_secs(15);
    while !done.get() {
        pump_for(Duration::from_millis(5));
        assert!(Instant::now() < end, "external source timed out");
    }
    window.close();
}

#[gtk::test]
fn slice3_external_move_waits_for_terminal_ack_over_real_x11() {
    adw::init().unwrap();
    if let Some(root) = std::env::var_os("TASK8_EXTERNAL_DND_SOURCE") {
        external_source(Path::new(&root));
        return;
    }
    // This is specifically the real-Xvfb gate; on a Wayland desktop use the
    // documented Xvfb command to exercise the cross-process wire protocol.
    if gtk::gdk::Display::default().unwrap().type_().name() != "GdkX11Display" {
        return;
    }
    let x = X11::new();
    for (case_index, mode) in [
        "success",
        "cancel",
        "failure",
        "conflict_skip",
        "partial",
        "partial_then_success",
    ]
    .into_iter()
    .enumerate()
    {
        let success = mode == "success";
        let fixture = tempfile::tempdir().unwrap();
        std::fs::write(fixture.path().join("source.txt"), "external data").unwrap();
        let destination = fixture.path().join("destination");
        std::fs::create_dir(&destination).unwrap();
        let view = FileView::new(Location::from_local_path(&destination));
        let case_token = format!("{}-{case_index}", std::process::id());
        let source_title = format!("Task8 external source {case_token}");
        let destination_title = format!("Task8 drop destination {case_token}");
        let window = gtk::Window::builder()
            .title(&destination_title)
            .default_width(300)
            .default_height(300)
            .build();
        window.set_child(Some(&view.widget));
        window.present();
        let pending = Rc::new(RefCell::new(None));
        let output = pending.clone();
        view.set_application_handler(move |command| *output.borrow_mut() = Some(command));
        let mut child = TestChild(std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "ui::window::dnd_tests::slice3_external_move_waits_for_terminal_ack_over_real_x11",
                "--nocapture",
            ])
            .env("TASK8_EXTERNAL_DND_SOURCE", fixture.path())
            .env("TASK8_EXTERNAL_DND_MODE", mode)
            .env("TASK8_EXTERNAL_DND_SOURCE_TITLE", &source_title)
            .spawn()
            .unwrap());
        let end = Instant::now() + Duration::from_secs(10);
        while x.window(&source_title).is_none() || !view.command_availability().can_paste {
            pump_for(Duration::from_millis(10));
            assert!(Instant::now() < end);
        }
        x.place(x.window(&source_title).unwrap(), 0);
        x.place(x.window(&destination_title).unwrap(), 400);
        x.motion(80, 200);
        x.button(true);
        for (a, b) in [(110, 200), (160, 210), (350, 220), (500, 250), (550, 300)] {
            x.motion(a, b);
        }
        x.button(false);
        while pending.borrow().is_none() {
            pump_for(Duration::from_millis(5));
            assert!(
                Instant::now() < end,
                "async target did not dispatch the external URI payload"
            );
        }
        let command = pending.borrow_mut().take().unwrap();
        assert_eq!(command.transfer, Some(ClipboardAction::Cut));
        assert!(
            command.source_deletes_on_success,
            "cross-process drop has no local GdkDrag"
        );
        assert!(
            !fixture.path().join("finished").exists(),
            "enqueue must not acknowledge MOVE"
        );
        assert!(fixture.path().join("source.txt").exists());
        let manager = OperationManager::default();
        // Deliberately keep the command pending across several GTK iterations.
        pump_for(Duration::from_millis(120));
        assert!(!fixture.path().join("finished").exists());
        if mode == "failure" {
            std::fs::remove_dir(&destination).unwrap();
        }
        if mode == "conflict_skip" {
            std::fs::write(destination.join("source.txt"), "keep existing").unwrap();
        }
        transfer_next(
            view.clone(),
            ClipboardPayload {
                action: ClipboardAction::Copy,
                locations: command.selection,
            },
            command.destination,
            manager.clone(),
            None,
            0,
            if mode == "cancel" {
                Some(ConflictDecision::CancelRemaining)
            } else {
                None
            },
            TransferBatch {
                completion: command.completion,
                ..Default::default()
            },
        );
        if mode == "conflict_skip" {
            let conflict = loop {
                pump_for(Duration::from_millis(5));
                assert!(Instant::now() < end);
                if let Some(dialog) = gtk::Window::list_toplevels()
                    .into_iter()
                    .filter_map(|w| w.downcast::<adw::MessageDialog>().ok())
                    .find(|d| {
                        d.heading().as_deref() == Some("An item with this name already exists")
                    })
                {
                    break dialog;
                }
            };
            pump_for(Duration::from_millis(100));
            assert!(
                !fixture.path().join("finished").exists(),
                "an unresolved conflict must not acknowledge MOVE"
            );
            assert!(fixture.path().join("source.txt").exists());
            conflict.response("skip");
        }
        while !fixture.path().join("finished").exists() {
            pump_for(Duration::from_millis(5));
            assert!(Instant::now() < end);
        }
        assert_eq!(
            std::fs::read_to_string(fixture.path().join("finished")).unwrap(),
            if success { "move" } else { "rejected" }
        );
        assert_eq!(fixture.path().join("source.txt").exists(), !success);
        assert_eq!(
            destination.join("source.txt").exists(),
            success || mode.starts_with("partial") || mode == "conflict_skip"
        );
        if mode == "conflict_skip" {
            assert_eq!(
                std::fs::read_to_string(destination.join("source.txt")).unwrap(),
                "keep existing"
            );
        }
        assert_eq!(manager.active_count(), 0);
        assert!(child.0.wait().unwrap().success());
        for dialog in gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<adw::MessageDialog>().ok())
        {
            dialog.close();
        }
        window.close();
        view.close();
        pump_for(Duration::from_millis(40));
    }
}
