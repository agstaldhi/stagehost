#[cfg(windows)]
mod win {
    use std::path::PathBuf;

    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{
        FileOpenDialog, FileSaveDialog, IFileDialog, IFileOpenDialog, IFileSaveDialog, IShellItem,
        FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS,
        SIGDN_FILESYSPATH,
    };

    struct ComGuard(bool);

    impl ComGuard {
        fn init() -> Option<Self> {
            // SAFETY: Standard STA initialization on the UI thread; matched by CoUninitialize.
            let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
            if hr.is_ok() {
                Some(Self(true))
            } else if hr.0 == windows::Win32::Foundation::RPC_E_CHANGED_MODE.0 {
                // Thread is already COM-initialized with MTA; proceed without calling CoUninitialize.
                Some(Self(false))
            } else {
                None
            }
        }
    }

    impl Drop for ComGuard {
        fn drop(&mut self) {
            if self.0 {
                // SAFETY: Paired with a successful CoInitializeEx in ComGuard::init.
                unsafe { CoUninitialize() };
            }
        }
    }

    fn to_wide_null(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn read_shell_item_path(item: &IShellItem) -> Option<PathBuf> {
        // SAFETY: SIGDN_FILESYSPATH allocates a CoTaskMem string freed cleanly below.
        let pwstr: PWSTR = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH).ok()? };
        let path_str = unsafe { pwstr.to_string().ok() };
        unsafe { CoTaskMemFree(Some(pwstr.0.cast())) };
        path_str.map(PathBuf::from)
    }

    fn configure_dialog(
        dialog: &IFileDialog,
        title: &str,
        filter_name: Option<&str>,
        filter_pattern: Option<&str>,
        extra_flags: windows::Win32::UI::Shell::FILEOPENDIALOGOPTIONS,
    ) {
        let title_w = to_wide_null(title);
        unsafe {
            let _ = dialog.SetTitle(PCWSTR(title_w.as_ptr()));
            if let Ok(mut opts) = dialog.GetOptions() {
                opts |= FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST | extra_flags;
                let _ = dialog.SetOptions(opts);
            }
            if let (Some(name), Some(spec)) = (filter_name, filter_pattern) {
                let name_w = to_wide_null(name);
                let spec_w = to_wide_null(spec);
                let filters = [COMDLG_FILTERSPEC {
                    pszName: PCWSTR(name_w.as_ptr()),
                    pszSpec: PCWSTR(spec_w.as_ptr()),
                }];
                let _ = dialog.SetFileTypes(&filters);
                let _ = dialog.SetFileTypeIndex(1);
            }
        }
    }

    pub fn pick_open_file(title: &str, filter_name: &str, filter_pattern: &str) -> Option<PathBuf> {
        let _com = ComGuard::init()?;
        let dialog: IFileOpenDialog =
            unsafe { CoCreateInstance(&FileOpenDialog, None, CLSCTX_ALL).ok()? };
        let base: IFileDialog = windows::core::Interface::cast(&dialog).ok()?;
        configure_dialog(
            &base,
            title,
            Some(filter_name),
            Some(filter_pattern),
            windows::Win32::UI::Shell::FILEOPENDIALOGOPTIONS(0),
        );
        unsafe { base.Show(None).ok()? };
        let item = unsafe { base.GetResult().ok()? };
        read_shell_item_path(&item)
    }

    pub fn pick_save_file(
        title: &str,
        default_name: &str,
        filter_name: &str,
        filter_pattern: &str,
    ) -> Option<PathBuf> {
        let _com = ComGuard::init()?;
        let dialog: IFileSaveDialog =
            unsafe { CoCreateInstance(&FileSaveDialog, None, CLSCTX_ALL).ok()? };
        let base: IFileDialog = windows::core::Interface::cast(&dialog).ok()?;
        configure_dialog(
            &base,
            title,
            Some(filter_name),
            Some(filter_pattern),
            FOS_OVERWRITEPROMPT,
        );
        let default_w = to_wide_null(default_name);
        unsafe {
            let _ = base.SetFileName(PCWSTR(default_w.as_ptr()));
            base.Show(None).ok()?;
        }
        let item = unsafe { base.GetResult().ok()? };
        read_shell_item_path(&item)
    }

    pub fn pick_folder(title: &str) -> Option<PathBuf> {
        let _com = ComGuard::init()?;
        let dialog: IFileOpenDialog =
            unsafe { CoCreateInstance(&FileOpenDialog, None, CLSCTX_ALL).ok()? };
        let base: IFileDialog = windows::core::Interface::cast(&dialog).ok()?;
        configure_dialog(&base, title, None, None, FOS_PICKFOLDERS);
        unsafe { base.Show(None).ok()? };
        let item = unsafe { base.GetResult().ok()? };
        read_shell_item_path(&item)
    }
}

#[cfg(windows)]
pub use win::{pick_folder, pick_open_file, pick_save_file};

#[cfg(not(windows))]
pub fn pick_open_file(
    _title: &str,
    _filter_name: &str,
    _filter_pattern: &str,
) -> Option<std::path::PathBuf> {
    None
}

#[cfg(not(windows))]
pub fn pick_save_file(
    _title: &str,
    _default_name: &str,
    _filter_name: &str,
    _filter_pattern: &str,
) -> Option<std::path::PathBuf> {
    None
}

#[cfg(not(windows))]
pub fn pick_folder(_title: &str) -> Option<std::path::PathBuf> {
    None
}
