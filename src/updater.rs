//! In-app updates via Sparkle.
//!
//! `scripts/bundle.sh` embeds Sparkle.framework at Contents/Frameworks, and
//! this module loads it at runtime instead of linking it, so a bare `cargo
//! run` binary simply runs without an updater. Sparkle still owns update
//! discovery, download, signature verification, installation, and relaunch.
//! Waku's routing user driver keeps automatic checks in the sidebar, but
//! forwards an explicit Check for Updates action to Sparkle's standard user
//! driver so the original updater window still appears when requested.
//!
//! Debug builds stay dormant so the dev watcher's app never offers to replace
//! itself with a production build. `WAKU_PREVIEW_UPDATE=1` fakes only the
//! automatic sidebar result while retaining the real Sparkle flow for the
//! Check for Updates menu; `WAKU_FORCE_UPDATER=1` exercises everything for
//! real from a debug bundle.

use gpui::Global;

/// App-wide handle to the updater, if this build can update itself.
pub struct UpdaterState(pub Option<Updater>);

impl Global for UpdaterState {}

/// The compact state rendered by Waku. Update details remain owned by
/// Sparkle and never enter a frame path.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum UpdateStatus {
    #[default]
    Idle,
    Available,
    Updating,
}

#[derive(Clone, Debug)]
#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
pub enum UpdaterEvent {
    StatusChanged(UpdateStatus),
    UpToDate,
    Failed(String),
}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use block2::{DynBlock, RcBlock};
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, NSObject, NSObjectProtocol};
    use objc2::{
        DefinedClass, MainThreadMarker, MainThreadOnly, define_class, extern_protocol, msg_send,
        sel,
    };
    use objc2_foundation::NSString;

    use super::{UpdateStatus, UpdaterEvent};

    const USER_UPDATE_CHOICE_INSTALL: isize = 1;
    const UPDATE_CHECK_USER_INITIATED: isize = 0;
    const UPDATE_CHECK_IN_BACKGROUND: isize = 1;
    const MANUAL_CHECK_MAX_RETRIES: u16 = 200;

    extern_protocol!(
        /// Dynamically loaded from the embedded Sparkle framework.
        unsafe trait SPUUserDriver: NSObjectProtocol {}
    );

    extern_protocol!(
        /// Only the update-cycle completion callback is implemented below.
        unsafe trait SPUUpdaterDelegate: NSObjectProtocol {}
    );

    struct PendingUpdate {
        appcast_item: Retained<AnyObject>,
        state: Retained<AnyObject>,
        reply: RcBlock<dyn Fn(isize)>,
    }

    struct UserDriverIvars {
        /// Explicit checks and the one-time automatic-check permission prompt
        /// use Sparkle's own windows. Scheduled checks stay inside Waku.
        standard_driver: Retained<AnyObject>,
        standard_presentation: Cell<bool>,
        standard_update_check: Cell<Option<isize>>,
        manual_check_requested: Rc<Cell<bool>>,
        manual_check_retry_count: Cell<u16>,
        pending_update: RefCell<Option<PendingUpdate>>,
        status: Rc<Cell<UpdateStatus>>,
        events: smol::channel::Sender<UpdaterEvent>,
    }

    define_class!(
        #[unsafe(super(NSObject))]
        #[name = "WakuSparkleUserDriver"]
        #[thread_kind = MainThreadOnly]
        #[ivars = UserDriverIvars]
        struct UserDriver;

        unsafe impl SPUUserDriver for UserDriver {
            #[unsafe(method(showUpdatePermissionRequest:reply:))]
            fn show_update_permission_request(
                &self,
                request: &AnyObject,
                reply: &DynBlock<dyn Fn(*mut AnyObject)>,
            ) {
                let _: () = unsafe {
                    msg_send![
                        &*self.ivars().standard_driver,
                        showUpdatePermissionRequest: request,
                        reply: reply
                    ]
                };
            }

            #[unsafe(method(showUserInitiatedUpdateCheckWithCancellation:))]
            fn show_user_initiated_update_check(&self, cancellation: &DynBlock<dyn Fn()>) {
                self.begin_standard_presentation(UPDATE_CHECK_USER_INITIATED);
                let _: () = unsafe {
                    msg_send![
                        &*self.ivars().standard_driver,
                        showUserInitiatedUpdateCheckWithCancellation: cancellation
                    ]
                };
            }

            #[unsafe(method(showUpdateFoundWithAppcastItem:state:reply:))]
            fn show_update_found(
                &self,
                appcast_item: &AnyObject,
                state: &AnyObject,
                reply: &DynBlock<dyn Fn(isize)>,
            ) {
                if self.ivars().manual_check_requested.replace(false) {
                    self.begin_standard_presentation(UPDATE_CHECK_IN_BACKGROUND);
                    self.show_update_found_with_standard_driver(appcast_item, state, reply);
                    return;
                }
                if self.uses_standard_presentation() {
                    self.show_update_found_with_standard_driver(appcast_item, state, reply);
                    return;
                }

                let appcast_item = unsafe {
                    Retained::retain(std::ptr::from_ref(appcast_item).cast_mut())
                        .expect("Sparkle supplied a non-null appcast item")
                };
                let state = unsafe {
                    Retained::retain(std::ptr::from_ref(state).cast_mut())
                        .expect("Sparkle supplied a non-null update state")
                };
                self.ivars().pending_update.replace(Some(PendingUpdate {
                    appcast_item,
                    state,
                    reply: reply.copy(),
                }));
                self.set_status(UpdateStatus::Available);
            }

            #[unsafe(method(showUpdateReleaseNotesWithDownloadData:))]
            fn show_update_release_notes(&self, download_data: &AnyObject) {
                if self.uses_standard_presentation() {
                    let _: () = unsafe {
                        msg_send![
                            &*self.ivars().standard_driver,
                            showUpdateReleaseNotesWithDownloadData: download_data
                        ]
                    };
                }
            }

            #[unsafe(method(showUpdateReleaseNotesFailedToDownloadWithError:))]
            fn show_update_release_notes_failed(&self, error: &AnyObject) {
                if self.uses_standard_presentation() {
                    let _: () = unsafe {
                        msg_send![
                            &*self.ivars().standard_driver,
                            showUpdateReleaseNotesFailedToDownloadWithError: error
                        ]
                    };
                }
            }

            #[unsafe(method(showUpdateNotFoundWithError:acknowledgement:))]
            fn show_update_not_found(
                &self,
                error: &AnyObject,
                acknowledgement: &DynBlock<dyn Fn()>,
            ) {
                if self.ivars().manual_check_requested.replace(false) {
                    self.begin_standard_presentation(UPDATE_CHECK_IN_BACKGROUND);
                }
                if self.uses_standard_presentation() {
                    let _: () = unsafe {
                        msg_send![
                            &*self.ivars().standard_driver,
                            showUpdateNotFoundWithError: error,
                            acknowledgement: acknowledgement
                        ]
                    };
                    return;
                }

                self.clear_update();
                self.send(UpdaterEvent::UpToDate);
                acknowledgement.call(());
            }

            #[unsafe(method(showUpdaterError:acknowledgement:))]
            fn show_updater_error(
                &self,
                error: &AnyObject,
                acknowledgement: &DynBlock<dyn Fn()>,
            ) {
                if self.ivars().manual_check_requested.replace(false) {
                    self.begin_standard_presentation(UPDATE_CHECK_IN_BACKGROUND);
                }
                if self.uses_standard_presentation() {
                    let _: () = unsafe {
                        msg_send![
                            &*self.ivars().standard_driver,
                            showUpdaterError: error,
                            acknowledgement: acknowledgement
                        ]
                    };
                    return;
                }

                self.clear_update();
                self.send(UpdaterEvent::Failed(error_description(error)));
                acknowledgement.call(());
            }

            #[unsafe(method(showDownloadInitiatedWithCancellation:))]
            fn show_download_initiated(&self, cancellation: &DynBlock<dyn Fn()>) {
                if self.uses_standard_presentation() {
                    let _: () = unsafe {
                        msg_send![
                            &*self.ivars().standard_driver,
                            showDownloadInitiatedWithCancellation: cancellation
                        ]
                    };
                    return;
                }
                self.set_status(UpdateStatus::Updating);
            }

            #[unsafe(method(showDownloadDidReceiveExpectedContentLength:))]
            fn show_expected_content_length(&self, expected_content_length: u64) {
                if self.uses_standard_presentation() {
                    let _: () = unsafe {
                        msg_send![
                            &*self.ivars().standard_driver,
                            showDownloadDidReceiveExpectedContentLength: expected_content_length
                        ]
                    };
                }
            }

            #[unsafe(method(showDownloadDidReceiveDataOfLength:))]
            fn show_downloaded_data(&self, length: u64) {
                if self.uses_standard_presentation() {
                    let _: () = unsafe {
                        msg_send![
                            &*self.ivars().standard_driver,
                            showDownloadDidReceiveDataOfLength: length
                        ]
                    };
                }
            }

            #[unsafe(method(showDownloadDidStartExtractingUpdate))]
            fn show_extracting_update(&self) {
                if self.uses_standard_presentation() {
                    let _: () = unsafe {
                        msg_send![&*self.ivars().standard_driver, showDownloadDidStartExtractingUpdate]
                    };
                    return;
                }
                self.set_status(UpdateStatus::Updating);
            }

            #[unsafe(method(showExtractionReceivedProgress:))]
            fn show_extraction_progress(&self, progress: f64) {
                if self.uses_standard_presentation() {
                    let _: () = unsafe {
                        msg_send![
                            &*self.ivars().standard_driver,
                            showExtractionReceivedProgress: progress
                        ]
                    };
                }
            }

            #[unsafe(method(showReadyToInstallAndRelaunch:))]
            fn show_ready_to_install(&self, reply: &DynBlock<dyn Fn(isize)>) {
                if self.uses_standard_presentation() {
                    let _: () = unsafe {
                        msg_send![
                            &*self.ivars().standard_driver,
                            showReadyToInstallAndRelaunch: reply
                        ]
                    };
                    return;
                }
                self.set_status(UpdateStatus::Updating);
                reply.call((USER_UPDATE_CHOICE_INSTALL,));
            }

            #[unsafe(method(showInstallingUpdateWithApplicationTerminated:retryTerminatingApplication:))]
            fn show_installing_update(
                &self,
                application_terminated: bool,
                retry_terminating_application: &DynBlock<dyn Fn()>,
            ) {
                if self.uses_standard_presentation() {
                    let _: () = unsafe {
                        msg_send![
                            &*self.ivars().standard_driver,
                            showInstallingUpdateWithApplicationTerminated: application_terminated,
                            retryTerminatingApplication: retry_terminating_application
                        ]
                    };
                    return;
                }
                self.set_status(UpdateStatus::Updating);
            }

            #[unsafe(method(showUpdateInstalledAndRelaunched:acknowledgement:))]
            fn show_update_installed(
                &self,
                relaunched: bool,
                acknowledgement: &DynBlock<dyn Fn()>,
            ) {
                if self.uses_standard_presentation() {
                    let _: () = unsafe {
                        msg_send![
                            &*self.ivars().standard_driver,
                            showUpdateInstalledAndRelaunched: relaunched,
                            acknowledgement: acknowledgement
                        ]
                    };
                    return;
                }
                acknowledgement.call(());
            }

            #[unsafe(method(dismissUpdateInstallation))]
            fn dismiss_update_installation(&self) {
                if self.uses_standard_presentation() {
                    let _: () = unsafe {
                        msg_send![&*self.ivars().standard_driver, dismissUpdateInstallation]
                    };
                    self.ivars().standard_presentation.set(false);
                    self.ivars().standard_update_check.set(None);
                }
                self.clear_update();
            }

            #[unsafe(method(showUpdateInFocus))]
            fn show_update_in_focus(&self) {
                if !self.uses_standard_presentation() && self.present_pending_update() {
                    return;
                }
                let _: () = unsafe {
                    msg_send![&*self.ivars().standard_driver, showUpdateInFocus]
                };
            }
        }

        impl UserDriver {
            #[unsafe(method(startRequestedStandardUpdateCheck:))]
            fn start_requested_standard_update_check(&self, updater: &AnyObject) {
                if !self.ivars().manual_check_requested.get() {
                    return;
                }
                if self.present_pending_update() {
                    self.ivars().manual_check_requested.set(false);
                    return;
                }

                let can_check: bool = unsafe { msg_send![updater, canCheckForUpdates] };
                if can_check {
                    self.ivars().manual_check_requested.set(false);
                    self.begin_standard_presentation(UPDATE_CHECK_USER_INITIATED);
                    let _: () = unsafe { msg_send![updater, checkForUpdates] };
                } else if self.ivars().manual_check_retry_count.get()
                    < MANUAL_CHECK_MAX_RETRIES
                {
                    self.ivars()
                        .manual_check_retry_count
                        .set(self.ivars().manual_check_retry_count.get() + 1);
                    self.schedule_requested_standard_check(updater, 0.05);
                } else {
                    self.ivars().manual_check_requested.set(false);
                    let _: () = unsafe {
                        msg_send![&*self.ivars().standard_driver, dismissUpdateInstallation]
                    };
                }
            }
        }

        unsafe impl SPUUpdaterDelegate for UserDriver {
            #[unsafe(method(updater:didFinishUpdateCycleForUpdateCheck:error:))]
            fn did_finish_update_cycle(
                &self,
                _updater: &AnyObject,
                update_check: isize,
                _error: Option<&AnyObject>,
            ) {
                if self.ivars().standard_update_check.get() == Some(update_check) {
                    self.ivars().standard_presentation.set(false);
                    self.ivars().standard_update_check.set(None);
                }
            }
        }

        unsafe impl NSObjectProtocol for UserDriver {}
    );

    impl UserDriver {
        fn new(
            mtm: MainThreadMarker,
            standard_driver: Retained<AnyObject>,
            status: Rc<Cell<UpdateStatus>>,
            events: smol::channel::Sender<UpdaterEvent>,
        ) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(UserDriverIvars {
                standard_driver,
                standard_presentation: Cell::new(false),
                standard_update_check: Cell::new(None),
                manual_check_requested: Rc::new(Cell::new(false)),
                manual_check_retry_count: Cell::new(0),
                pending_update: RefCell::new(None),
                status,
                events,
            });
            unsafe { msg_send![super(this), init] }
        }

        fn uses_standard_presentation(&self) -> bool {
            self.ivars().standard_presentation.get()
        }

        fn begin_standard_presentation(&self, update_check: isize) {
            self.ivars().standard_presentation.set(true);
            self.ivars().standard_update_check.set(Some(update_check));
        }

        fn schedule_requested_standard_check(&self, updater: &AnyObject, delay: f64) {
            let _: () = unsafe {
                msg_send![
                    self,
                    performSelector: sel!(startRequestedStandardUpdateCheck:),
                    withObject: updater,
                    afterDelay: delay
                ]
            };
        }

        fn show_update_found_with_standard_driver(
            &self,
            appcast_item: &AnyObject,
            state: &AnyObject,
            reply: &DynBlock<dyn Fn(isize)>,
        ) {
            let _: () = unsafe {
                msg_send![
                    &*self.ivars().standard_driver,
                    showUpdateFoundWithAppcastItem: appcast_item,
                    state: state,
                    reply: reply
                ]
            };
            // A result discovered by the silent checker still has
            // `state.userInitiated == false`, so the standard driver may apply
            // its gentle-reminder rules and leave the alert hidden. The menu
            // action is explicit, so bring the newly created alert forward.
            let _: () = unsafe { msg_send![&*self.ivars().standard_driver, showUpdateInFocus] };
        }

        /// Promote the result already held by a silent automatic check into
        /// Sparkle's standard updater window without discarding it and
        /// starting a second network request.
        fn present_pending_update(&self) -> bool {
            let Some(update) = self.ivars().pending_update.borrow_mut().take() else {
                return false;
            };

            self.begin_standard_presentation(UPDATE_CHECK_IN_BACKGROUND);
            self.set_status(UpdateStatus::Idle);
            self.show_update_found_with_standard_driver(
                &update.appcast_item,
                &update.state,
                &update.reply,
            );
            true
        }

        /// Show Sparkle's standard checking UI immediately, then start a real
        /// user-initiated check as soon as any silent automatic session has
        /// finished tearing down.
        fn request_standard_check(&self, updater: &AnyObject) -> bool {
            if self.present_pending_update() {
                return true;
            }
            if self.uses_standard_presentation() {
                let _: () = unsafe { msg_send![updater, checkForUpdates] };
                return true;
            }
            if self.ivars().manual_check_requested.get() {
                let _: () = unsafe { msg_send![&*self.ivars().standard_driver, showUpdateInFocus] };
                return true;
            }
            if self.ivars().status.get() == UpdateStatus::Updating {
                return false;
            }

            let can_check: bool = unsafe { msg_send![updater, canCheckForUpdates] };
            if can_check {
                self.begin_standard_presentation(UPDATE_CHECK_USER_INITIATED);
                let _: () = unsafe { msg_send![updater, checkForUpdates] };
                return true;
            }

            self.ivars().manual_check_requested.set(true);
            self.ivars().manual_check_retry_count.set(0);
            let manual_check_requested = self.ivars().manual_check_requested.clone();
            let cancellation: RcBlock<dyn Fn()> = RcBlock::new(move || {
                manual_check_requested.set(false);
            });
            let _: () = unsafe {
                msg_send![
                    &*self.ivars().standard_driver,
                    showUserInitiatedUpdateCheckWithCancellation: &*cancellation
                ]
            };

            self.schedule_requested_standard_check(updater, 0.0);
            true
        }

        fn send(&self, event: UpdaterEvent) {
            let _ = self.ivars().events.try_send(event);
        }

        fn set_status(&self, status: UpdateStatus) {
            if self.ivars().status.replace(status) != status {
                self.send(UpdaterEvent::StatusChanged(status));
            }
        }

        fn clear_update(&self) {
            self.ivars().pending_update.borrow_mut().take();
            self.set_status(UpdateStatus::Idle);
        }

        fn install_available_update(&self) -> bool {
            if self.uses_standard_presentation() {
                return false;
            }
            let Some(update) = self.ivars().pending_update.borrow_mut().take() else {
                return false;
            };
            self.set_status(UpdateStatus::Updating);
            update.reply.call((USER_UPDATE_CHOICE_INSTALL,));
            true
        }
    }

    pub struct Updater {
        updater: Option<Retained<AnyObject>>,
        user_driver: Option<Retained<UserDriver>>,
        status: Rc<Cell<UpdateStatus>>,
        events: smol::channel::Receiver<UpdaterEvent>,
        preview_events: Option<smol::channel::Sender<UpdaterEvent>>,
    }

    impl Updater {
        /// Load Sparkle and start its updater. Returns `None` when this build
        /// cannot update itself: debug builds unless forced, and binaries
        /// running outside a bundle with an embedded framework.
        pub fn init() -> Option<Self> {
            let preview = cfg!(debug_assertions)
                && std::env::var_os("WAKU_PREVIEW_UPDATE").is_some_and(|value| value == "1");
            let forced = std::env::var_os("WAKU_FORCE_UPDATER").is_some_and(|value| value == "1");
            if cfg!(debug_assertions) && !forced && !preview {
                return None;
            }

            let mtm = MainThreadMarker::new()?;
            let library = sparkle_library_path()?;
            let library_c =
                std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(library.as_os_str()))
                    .ok()?;
            let handle = unsafe { libc::dlopen(library_c.as_ptr(), libc::RTLD_NOW) };
            if handle.is_null() {
                let reason = unsafe { libc::dlerror() };
                let reason = if reason.is_null() {
                    "unknown dlopen failure".into()
                } else {
                    unsafe { std::ffi::CStr::from_ptr(reason) }
                        .to_string_lossy()
                        .into_owned()
                };
                eprintln!("Waku updater: failed to load Sparkle: {reason}");
                return None;
            }

            let bundle_class = AnyClass::get(c"NSBundle")?;
            let updater_class = AnyClass::get(c"SPUUpdater")?;
            let standard_driver_class = AnyClass::get(c"SPUStandardUserDriver")?;
            let main_bundle: *mut AnyObject = unsafe { msg_send![bundle_class, mainBundle] };
            if main_bundle.is_null() {
                return None;
            }

            let standard_driver = unsafe {
                let allocated: *mut AnyObject = msg_send![standard_driver_class, alloc];
                let initialized: *mut AnyObject = msg_send![
                    allocated,
                    initWithHostBundle: main_bundle,
                    delegate: std::ptr::null_mut::<AnyObject>()
                ];
                Retained::from_raw(initialized)?
            };

            let status = Rc::new(Cell::new(if preview {
                UpdateStatus::Available
            } else {
                UpdateStatus::Idle
            }));
            let (event_tx, events) = smol::channel::unbounded();
            let preview_events = preview.then(|| event_tx.clone());
            let user_driver = UserDriver::new(mtm, standard_driver, status.clone(), event_tx);
            let updater = unsafe {
                let allocated: *mut AnyObject = msg_send![updater_class, alloc];
                let initialized: *mut AnyObject = msg_send![
                    allocated,
                    initWithHostBundle: main_bundle,
                    applicationBundle: main_bundle,
                    userDriver: &*user_driver,
                    delegate: &*user_driver
                ];
                Retained::from_raw(initialized)?
            };

            let started: bool = unsafe {
                msg_send![
                    &*updater,
                    startUpdater: std::ptr::null_mut::<*mut AnyObject>()
                ]
            };
            if !started {
                eprintln!("Waku updater: Sparkle rejected its updater configuration");
                return None;
            }

            let updater = Self {
                updater: Some(updater),
                user_driver: Some(user_driver),
                status,
                events,
                preview_events,
            };

            // Starting only arms the scheduled checker, which stays quiet
            // until its interval has elapsed since the last check. Force one
            // silent check per launch once the user has consented.
            if !preview && updater.automatically_checks_for_updates() {
                let sparkle = updater
                    .updater
                    .as_ref()
                    .expect("Sparkle updater initialized");
                let _: () = unsafe { msg_send![&**sparkle, checkForUpdatesInBackground] };
            }

            Some(updater)
        }

        /// Run a user-initiated check through Sparkle's standard updater UI.
        pub fn check_for_updates(&self) {
            if let Some(updater) = &self.updater {
                // Preview mode fakes only the automatic result. Hide that
                // placeholder before handing the explicit action to Sparkle.
                if self.preview_events.is_some() {
                    self.set_preview_status(UpdateStatus::Idle);
                }
                if let Some(user_driver) = &self.user_driver {
                    user_driver.request_standard_check(updater);
                }
            } else {
                self.set_preview_status(UpdateStatus::Available);
            }
        }

        pub fn install_available_update(&self) -> bool {
            if self
                .user_driver
                .as_ref()
                .is_some_and(|user_driver| user_driver.install_available_update())
            {
                return true;
            }
            if self.preview_events.is_some() && self.status.get() == UpdateStatus::Available {
                self.set_preview_status(UpdateStatus::Updating);
                true
            } else {
                false
            }
        }

        pub fn status(&self) -> UpdateStatus {
            self.status.get()
        }

        pub fn events(&self) -> smol::channel::Receiver<UpdaterEvent> {
            self.events.clone()
        }

        /// Whether Sparkle checks for updates on its own schedule. Sparkle
        /// owns the persisted value in this app's user defaults.
        pub fn automatically_checks_for_updates(&self) -> bool {
            self.updater.as_ref().is_some_and(|updater| unsafe {
                msg_send![&**updater, automaticallyChecksForUpdates]
            })
        }

        pub fn set_automatically_checks_for_updates(&self, enabled: bool) {
            if let Some(updater) = &self.updater {
                let _: () =
                    unsafe { msg_send![&**updater, setAutomaticallyChecksForUpdates: enabled] };
            }
        }

        #[cfg(test)]
        fn preview() -> Self {
            let status = Rc::new(Cell::new(UpdateStatus::Available));
            let (preview_events, events) = smol::channel::unbounded();
            Self {
                updater: None,
                user_driver: None,
                status,
                events,
                preview_events: Some(preview_events),
            }
        }

        fn set_preview_status(&self, status: UpdateStatus) {
            if self.status.replace(status) != status
                && let Some(events) = &self.preview_events
            {
                let _ = events.try_send(UpdaterEvent::StatusChanged(status));
            }
        }
    }

    fn error_description(error: &AnyObject) -> String {
        let description: *mut NSString = unsafe { msg_send![error, localizedDescription] };
        unsafe { description.as_ref() }
            .map(ToString::to_string)
            .unwrap_or_else(|| "Unknown updater error".to_owned())
    }

    /// The embedded framework's dylib next to the running executable
    /// (Contents/MacOS/Waku → Contents/Frameworks/Sparkle.framework/Sparkle).
    fn sparkle_library_path() -> Option<std::path::PathBuf> {
        let executable = std::env::current_exe().ok()?;
        let contents = executable.parent()?.parent()?;
        let library = contents.join("Frameworks/Sparkle.framework/Sparkle");
        library.exists().then_some(library)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use objc2::ClassType;

        #[test]
        fn routing_user_driver_satisfies_sparkle_protocols() {
            let target_dir = std::env::var_os("CARGO_TARGET_DIR")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target"));
            let library = target_dir
                .join("debug/Waku Debug.app/Contents/Frameworks/Sparkle.framework/Sparkle");
            if !library.exists() {
                return;
            }

            let library_c =
                std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(library.as_os_str()))
                    .expect("Sparkle path must not contain a null byte");
            let handle = unsafe { libc::dlopen(library_c.as_ptr(), libc::RTLD_NOW) };
            assert!(!handle.is_null(), "embedded Sparkle framework must load");

            // Registration validates every required SPUUserDriver selector
            // against the live protocol and panics if one is misplaced.
            let _ = UserDriver::class();
        }

        #[test]
        fn preview_update_switches_from_available_to_spinner() {
            let updater = Updater::preview();
            assert_eq!(updater.status(), UpdateStatus::Available);
            assert!(updater.install_available_update());
            assert_eq!(updater.status(), UpdateStatus::Updating);
            assert!(matches!(
                updater.events().try_recv(),
                Ok(UpdaterEvent::StatusChanged(UpdateStatus::Updating))
            ));
        }
    }
}

#[cfg(target_os = "macos")]
pub use macos::Updater;

/// Reading a Sparkle appcast.
///
/// Kept off the platform modules so the ordering and parsing rules — the part
/// that decides which build a user is offered — are exercised by `cargo test`
/// on every host, not only on the one that ships an updater.
#[cfg(any(target_os = "windows", test))]
mod feed {
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(super) struct AppcastItem {
        pub(super) version: String,
        pub(super) url: String,
        pub(super) signature: String,
        pub(super) length: Option<u64>,
    }

    /// The newest signed item in a Sparkle appcast.
    ///
    /// `scripts/appcast-windows.ts` writes this feed, so the shape is a
    /// contract rather than arbitrary XML: one `<item>` per release, each with
    /// a `sparkle:shortVersionString` and a signed `<enclosure>`. Items whose
    /// signature is missing are ignored rather than trusted.
    pub(super) fn newest_item(feed: &str) -> Option<AppcastItem> {
        feed.split("<item>")
            .skip(1)
            .filter_map(|item| {
                let item = item.split("</item>").next()?;
                let enclosure = item.split_once("<enclosure")?.1.split_once('>')?.0;
                Some(AppcastItem {
                    version: element(item, "sparkle:shortVersionString")
                        .or_else(|| attribute(enclosure, "sparkle:shortVersionString"))?,
                    url: attribute(enclosure, "url")?,
                    signature: attribute(enclosure, "sparkle:edSignature")?,
                    length: attribute(enclosure, "length").and_then(|it| it.parse().ok()),
                })
            })
            .max_by(|left, right| compare_versions(&left.version, &right.version))
    }

    fn attribute(tag: &str, name: &str) -> Option<String> {
        let needle = format!("{name}=\"");
        let value = tag.split_once(&needle)?.1.split_once('"')?.0;
        (!value.is_empty()).then(|| value.to_owned())
    }

    fn element(item: &str, name: &str) -> Option<String> {
        let value = item
            .split_once(&format!("<{name}>"))?
            .1
            .split_once(&format!("</{name}>"))?
            .0
            .trim();
        (!value.is_empty()).then(|| value.to_owned())
    }

    pub(super) fn is_newer(candidate: &str, current: &str) -> bool {
        compare_versions(candidate, current) == std::cmp::Ordering::Greater
    }

    /// Compare dotted release numbers field by field. Waku's versions are
    /// plain `major.minor.patch`; anything after a `-` or `+` is build
    /// metadata and is not ordered.
    fn compare_versions(left: &str, right: &str) -> std::cmp::Ordering {
        fn fields(version: &str) -> impl Iterator<Item = u64> + '_ {
            version
                .split(['-', '+'])
                .next()
                .unwrap_or(version)
                .split('.')
                .map(|field| field.trim().parse::<u64>().unwrap_or(0))
        }

        let mut left = fields(left);
        let mut right = fields(right);
        loop {
            match (left.next(), right.next()) {
                (None, None) => return std::cmp::Ordering::Equal,
                (left, right) => {
                    let ordering = left.unwrap_or(0).cmp(&right.unwrap_or(0));
                    if ordering != std::cmp::Ordering::Equal {
                        return ordering;
                    }
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        const FEED: &str = r#"<?xml version="1.0" standalone="yes"?>
<rss xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle" version="2.0">
  <channel>
    <item>
      <title>0.1.4</title>
      <sparkle:shortVersionString>0.1.4</sparkle:shortVersionString>
      <enclosure url="https://releases.waku.sh/Waku-0.1.4-x86_64-Setup.exe" length="1024" type="application/octet-stream" sparkle:edSignature="oldsig" />
    </item>
    <item>
      <title>0.2.0</title>
      <sparkle:shortVersionString>0.2.0</sparkle:shortVersionString>
      <enclosure url="https://releases.waku.sh/Waku-0.2.0-x86_64-Setup.exe" length="2048" type="application/octet-stream" sparkle:edSignature="newsig" />
    </item>
  </channel>
</rss>"#;

        #[test]
        fn the_newest_signed_item_wins_regardless_of_feed_order() {
            let item = newest_item(FEED).expect("feed has signed items");

            assert_eq!(item.version, "0.2.0");
            assert_eq!(item.signature, "newsig");
            assert_eq!(item.length, Some(2048));
            assert!(item.url.ends_with("Waku-0.2.0-x86_64-Setup.exe"));
        }

        #[test]
        fn an_unsigned_enclosure_is_never_offered() {
            let unsigned = FEED.replace(" sparkle:edSignature=\"newsig\"", "");

            let item = newest_item(&unsigned).expect("the signed item remains");

            assert_eq!(item.version, "0.1.4");
        }

        #[test]
        fn a_feed_without_signed_items_offers_nothing() {
            assert_eq!(newest_item("<rss></rss>"), None);
            assert_eq!(newest_item(""), None);
        }

        #[test]
        fn versions_compare_field_by_field_not_lexically() {
            assert!(is_newer("0.10.0", "0.9.0"));
            assert!(is_newer("0.1.10", "0.1.9"));
            assert!(!is_newer("0.1.4", "0.1.4"));
            assert!(!is_newer("0.1.3", "0.1.4"));
            // A shorter version names the same release, not an older one.
            assert!(!is_newer("1.2", "1.2.0"));
            assert!(is_newer("1.2.1", "1.2"));
            // Build metadata never makes a release newer than itself.
            assert!(!is_newer("1.2.0+build.7", "1.2.0"));
        }
    }
}

/// In-app updates on Windows.
///
/// There is no Sparkle to hand the work to, so this module runs the same
/// contract itself: read a Sparkle-format appcast, compare versions, download
/// the installer, verify its EdDSA signature against the very key
/// `SUPublicEDKey` names, and hand it to Inno Setup in silent mode. The
/// installer closes the running app, replaces it, and starts it again.
///
/// Nothing here touches the UI thread. Checks and downloads run on their own
/// threads and report back through the same `UpdaterEvent` channel the macOS
/// driver uses, so the sidebar footer and the Settings toggle behave
/// identically on both platforms.
#[cfg(target_os = "windows")]
mod windows {
    use std::io::Write as _;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use base64::Engine as _;
    use ed25519_dalek::{Signature, VerifyingKey};

    use super::feed::{self, AppcastItem};
    use super::{UpdateStatus, UpdaterEvent};

    /// One feed per architecture. A Sparkle appcast has no way to say which
    /// binary an item is for, and guessing from the enclosure filename would
    /// be a contract hiding in a string.
    #[cfg(target_arch = "aarch64")]
    const FEED_URL: &str = "https://releases.waku.sh/appcast-windows-aarch64.xml";
    #[cfg(not(target_arch = "aarch64"))]
    const FEED_URL: &str = "https://releases.waku.sh/appcast-windows-x86_64.xml";

    /// Read out of `resources/Info.plist` by the build script, so macOS and
    /// Windows cannot end up trusting different keys.
    const PUBLIC_ED_KEY: &str = env!("WAKU_SPARKLE_PUBLIC_ED_KEY");

    /// Windows 10 1803 and later ship curl in System32. The absolute path
    /// keeps a shadowed `curl` on `PATH` out of the update path; the download
    /// is trusted through its signature, never through its transport.
    const CURL_PATH: &str = r"C:\Windows\System32\curl.exe";

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const MAX_FEED_BYTES: usize = 1024 * 1024;
    const MAX_INSTALLER_BYTES: u64 = 512 * 1024 * 1024;

    /// A verified installer on disk, ready to run.
    struct StagedUpdate {
        installer: PathBuf,
    }

    pub struct Updater {
        status: Arc<Mutex<UpdateStatus>>,
        staged: Arc<Mutex<Option<StagedUpdate>>>,
        /// A check is running. Separate from `status` because a silent check
        /// is deliberately invisible, so the published status cannot be what
        /// keeps two checks from overlapping.
        checking: Arc<AtomicBool>,
        /// Whether the running check reports its outcome. An explicit request
        /// that lands while a silent one is in flight sets it rather than
        /// being dropped, so Check for Updates still answers.
        explicit_check: Arc<AtomicBool>,
        automatic: Arc<AtomicBool>,
        preference_path: PathBuf,
        events: smol::channel::Sender<UpdaterEvent>,
        receiver: smol::channel::Receiver<UpdaterEvent>,
    }

    impl Updater {
        pub fn init() -> Option<Self> {
            // A debug build must never offer to replace the watcher's app
            // with a production install.
            let forced = std::env::var_os("WAKU_FORCE_UPDATER").is_some_and(|value| value == "1");
            if cfg!(debug_assertions) && !forced {
                return None;
            }
            if verifying_key().is_none() {
                eprintln!("Waku updater: SUPublicEDKey is not a valid ed25519 key");
                return None;
            }

            let preference_path = preference_path()?;
            let automatic = Arc::new(AtomicBool::new(read_automatic_preference(&preference_path)));
            let (events, receiver) = smol::channel::unbounded();
            let updater = Self {
                status: Arc::new(Mutex::new(UpdateStatus::Idle)),
                staged: Arc::new(Mutex::new(None)),
                checking: Arc::new(AtomicBool::new(false)),
                explicit_check: Arc::new(AtomicBool::new(false)),
                automatic,
                preference_path,
                events,
                receiver,
            };

            // Sparkle arms a scheduled checker on macOS; here one silent
            // check per launch is the whole schedule.
            if updater.automatically_checks_for_updates() {
                updater.start_check(false);
            }
            Some(updater)
        }

        /// A user-initiated check. Unlike the silent one, it reports both
        /// "already current" and failures.
        pub fn check_for_updates(&self) {
            self.start_check(true);
        }

        /// `user_initiated` decides only what is reported at the end. A check
        /// is never a state the sidebar renders — its button announces a ready
        /// update, not the poll that looks for one — so neither the launch
        /// check nor an explicit one puts a spinner in the footer. macOS is
        /// the same: Sparkle's own window carries an explicit check's
        /// progress, and its scheduled checks show nothing at all.
        fn start_check(&self, user_initiated: bool) {
            if self.status() == UpdateStatus::Updating {
                return;
            }
            if self
                .checking
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                // The launch check is in flight. An explicit request adopts
                // it so the menu still gets an answer, rather than being
                // dropped for the second or so that check takes.
                if user_initiated {
                    self.explicit_check.store(true, Ordering::Relaxed);
                }
                return;
            }
            self.explicit_check.store(user_initiated, Ordering::Relaxed);

            let status = self.status.clone();
            let staged = self.staged.clone();
            let checking = self.checking.clone();
            let explicit_check = self.explicit_check.clone();
            let events = self.events.clone();
            let publish = move |next: UpdateStatus| {
                let mut status = status
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                // A check leaves the status alone while it runs, so most of
                // its outcomes are not transitions and must not repaint.
                if *status == next {
                    return;
                }
                *status = next;
                let _ = events.try_send(UpdaterEvent::StatusChanged(next));
            };
            let events = self.events.clone();
            let spawned = std::thread::Builder::new()
                .name("waku-updater-check".into())
                .spawn(move || {
                    let outcome = fetch_and_stage();
                    // Read once the work is done, so a request that arrived
                    // meanwhile is honored.
                    let report = explicit_check.load(Ordering::Relaxed);
                    match outcome {
                        Ok(Some(installer)) => {
                            *staged
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                                Some(StagedUpdate { installer });
                            publish(UpdateStatus::Available);
                        }
                        Ok(None) => {
                            publish(UpdateStatus::Idle);
                            if report {
                                let _ = events.try_send(UpdaterEvent::UpToDate);
                            }
                        }
                        Err(error) => {
                            publish(UpdateStatus::Idle);
                            if report {
                                let _ = events.try_send(UpdaterEvent::Failed(error.to_string()));
                            } else {
                                eprintln!("Waku updater: {error}");
                            }
                        }
                    }
                    checking.store(false, Ordering::Release);
                });
            if spawned.is_err() {
                self.checking.store(false, Ordering::Release);
            }
        }

        /// Run the staged installer and leave. Inno Setup closes this process,
        /// replaces it in place, and starts the new build.
        pub fn install_available_update(&self) -> bool {
            let installer = {
                let mut staged = self
                    .staged
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                match staged.take() {
                    Some(update) => update.installer,
                    None => return false,
                }
            };

            // `/DIR` pins the update to the copy that is running, so a
            // portable unzip updates itself in place instead of sprouting a
            // second install under the default directory.
            let mut command = std::process::Command::new(&installer);
            command.args(["/SILENT", "/NORESTART", "/SP-"]);
            if let Some(directory) = install_directory() {
                command.arg(format!("/DIR={}", directory.display()));
            }
            {
                use std::os::windows::process::CommandExt as _;
                command.creation_flags(CREATE_NO_WINDOW);
            }
            match command.spawn() {
                Ok(_) => {
                    self.set_status(UpdateStatus::Updating);
                    true
                }
                Err(error) => {
                    let _ = self
                        .events
                        .try_send(UpdaterEvent::Failed(format!("{installer:?}: {error}")));
                    false
                }
            }
        }

        pub fn status(&self) -> UpdateStatus {
            *self
                .status
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        }

        pub fn events(&self) -> smol::channel::Receiver<UpdaterEvent> {
            self.receiver.clone()
        }

        pub fn automatically_checks_for_updates(&self) -> bool {
            self.automatic.load(Ordering::Relaxed)
        }

        pub fn set_automatically_checks_for_updates(&self, enabled: bool) {
            if self.automatic.swap(enabled, Ordering::Relaxed) == enabled {
                return;
            }
            let path = self.preference_path.clone();
            // A settings toggle must not wait on the filesystem.
            let _ = std::thread::Builder::new()
                .name("waku-updater-preference".into())
                .spawn(move || write_automatic_preference(&path, enabled));
            if enabled {
                self.start_check(false);
            }
        }

        fn set_status(&self, next: UpdateStatus) {
            let mut status = self
                .status
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if *status == next {
                return;
            }
            *status = next;
            let _ = self.events.try_send(UpdaterEvent::StatusChanged(next));
        }
    }

    /// Resolve the feed, and stage the installer when it names a newer build.
    fn fetch_and_stage() -> anyhow::Result<Option<PathBuf>> {
        let document = http_get(FEED_URL)?;
        let Some(item) = feed::newest_item(&document) else {
            anyhow::bail!("the update feed has no signed release");
        };
        if !feed::is_newer(&item.version, env!("CARGO_PKG_VERSION")) {
            return Ok(None);
        }
        Ok(Some(download_and_verify(&item)?))
    }

    fn download_and_verify(item: &AppcastItem) -> anyhow::Result<PathBuf> {
        let key = verifying_key().ok_or_else(|| anyhow::anyhow!("SUPublicEDKey is unusable"))?;
        let signature = base64::engine::general_purpose::STANDARD
            .decode(item.signature.trim())
            .ok()
            .and_then(|bytes| <[u8; 64]>::try_from(bytes).ok())
            .map(|bytes| Signature::from_bytes(&bytes))
            .ok_or_else(|| anyhow::anyhow!("update signature is malformed"))?;

        let directory = std::env::temp_dir().join(format!(
            "waku-update-{}-{}",
            item.version,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory)?;
        let installer = directory.join("Waku-Setup.exe");

        curl(&["-fsSL", "--max-time", "600", "-o"], &installer, &item.url)?;

        let metadata = std::fs::metadata(&installer)?;
        if metadata.len() > MAX_INSTALLER_BYTES {
            anyhow::bail!("update installer is implausibly large");
        }
        if item.length.is_some_and(|length| length != metadata.len()) {
            anyhow::bail!("update installer does not match the length the feed declared");
        }
        let bytes = std::fs::read(&installer)?;
        key.verify_strict(&bytes, &signature)
            .map_err(|_| anyhow::anyhow!("update installer failed signature verification"))?;
        Ok(installer)
    }

    fn verifying_key() -> Option<VerifyingKey> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(PUBLIC_ED_KEY.trim())
            .ok()?;
        VerifyingKey::from_bytes(&<[u8; 32]>::try_from(bytes).ok()?).ok()
    }

    fn http_get(url: &str) -> anyhow::Result<String> {
        let mut command = std::process::Command::new(CURL_PATH);
        command
            .args(["-fsSL", "--max-time", "30", url])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        {
            use std::os::windows::process::CommandExt as _;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let output = command.output()?;
        if !output.status.success() {
            anyhow::bail!(
                "could not reach the update feed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        if output.stdout.len() > MAX_FEED_BYTES {
            anyhow::bail!("the update feed is implausibly large");
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn curl(arguments: &[&str], destination: &Path, url: &str) -> anyhow::Result<()> {
        let mut command = std::process::Command::new(CURL_PATH);
        command
            .args(arguments)
            .arg(destination)
            .arg(url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        {
            use std::os::windows::process::CommandExt as _;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let output = command.output()?;
        anyhow::ensure!(
            output.status.success(),
            "could not download the update: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(())
    }

    fn install_directory() -> Option<PathBuf> {
        Some(std::env::current_exe().ok()?.parent()?.to_path_buf())
    }

    fn preference_path() -> Option<PathBuf> {
        Some(
            dirs::data_local_dir()?
                .join(waku_protocol::identity::DATA_DIRECTORY_NAME)
                .join("updater.json"),
        )
    }

    /// Sparkle's macOS default is to check automatically; match it, and treat
    /// an unreadable or absent file as "not answered yet".
    fn read_automatic_preference(path: &Path) -> bool {
        let Ok(contents) = std::fs::read_to_string(path) else {
            return true;
        };
        serde_json::from_str::<serde_json::Value>(&contents)
            .ok()
            .and_then(|value| value.get("automatic")?.as_bool())
            .unwrap_or(true)
    }

    fn write_automatic_preference(path: &Path, enabled: bool) {
        let Some(directory) = path.parent() else {
            return;
        };
        if std::fs::create_dir_all(directory).is_err() {
            return;
        }
        // Replace through a temporary file so a crash mid-write cannot leave
        // the preference unreadable.
        let temporary = path.with_extension("json.tmp");
        let written = std::fs::File::create(&temporary).and_then(|mut file| {
            file.write_all(format!("{{\n  \"automatic\": {enabled}\n}}\n").as_bytes())?;
            file.sync_all()
        });
        if written.is_ok() {
            let _ = std::fs::rename(&temporary, path);
        } else {
            let _ = std::fs::remove_file(&temporary);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_embedded_public_key_is_a_usable_ed25519_key() {
            assert!(verifying_key().is_some());
        }

        /// The interop the whole update path rests on: macOS signs with
        /// Sparkle's `sign_update`, Windows releases sign with Node's Ed25519
        /// in `scripts/appcast-windows.ts`, and this verifies both. The vector
        /// below came from that script with a throwaway key.
        #[test]
        fn a_signature_from_the_release_script_verifies_here() {
            const PUBLIC: &str = "7gZ3dbx+MPQD4vc2dk7olL9QU66JIjpJ1iqNNafU2lQ=";
            const SIGNATURE: &str = "eBIPKGvQSxFIVNwOzNjzHYs/AGiYFIe3pGulv0TeocoMN0+0l28OJZrlJ2ZuQnNBfif10VW3virGo+7GP3TwCw==";
            const PAYLOAD: &[u8] = b"Waku-0.0.0-x86_64-Setup.exe contents";

            let decode = |value: &str| {
                base64::engine::general_purpose::STANDARD
                    .decode(value)
                    .expect("test vector is valid base64")
            };
            let key = VerifyingKey::from_bytes(
                &<[u8; 32]>::try_from(decode(PUBLIC)).expect("32-byte public key"),
            )
            .expect("test vector is a valid key");
            let signature =
                Signature::from_bytes(&<[u8; 64]>::try_from(decode(SIGNATURE)).expect("64 bytes"));

            key.verify_strict(PAYLOAD, &signature)
                .expect("a release-script signature must verify");
            assert!(
                key.verify_strict(b"tampered installer", &signature)
                    .is_err(),
                "a modified download must not verify"
            );
        }

        #[test]
        fn an_absent_preference_file_leaves_automatic_checks_on() {
            let directory = std::env::temp_dir()
                .join(format!("waku-updater-preference-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&directory);
            let path = directory.join("updater.json");

            assert!(read_automatic_preference(&path));

            write_automatic_preference(&path, false);
            assert!(!read_automatic_preference(&path));
            write_automatic_preference(&path, true);
            assert!(read_automatic_preference(&path));

            let _ = std::fs::remove_dir_all(&directory);
        }
    }
}

#[cfg(target_os = "windows")]
pub use windows::Updater;

/// Linux has no updater yet — `install.sh` re-run is the upgrade path. This
/// stub is the seam where an implementation slots in; callers already treat
/// `None` as "no updater".
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub struct Updater;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
impl Updater {
    pub fn init() -> Option<Self> {
        None
    }

    pub fn check_for_updates(&self) {}

    pub fn install_available_update(&self) -> bool {
        false
    }

    pub fn status(&self) -> UpdateStatus {
        UpdateStatus::Idle
    }

    pub fn events(&self) -> smol::channel::Receiver<UpdaterEvent> {
        let (_tx, rx) = smol::channel::unbounded();
        rx
    }

    pub fn automatically_checks_for_updates(&self) -> bool {
        false
    }

    pub fn set_automatically_checks_for_updates(&self, _enabled: bool) {}
}
