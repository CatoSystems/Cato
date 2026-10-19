use std::ffi::{c_char, CStr};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
    Elf,
    Mingw,
    Darwin,
}

#[cfg(not(feature = "link"))]
pub fn link(_flavor: Flavor, _argv: &[&CStr]) -> Result<(), String> {
    unreachable!("cato-lld-sys built without the link feature")
}

#[cfg(feature = "link")]
extern "C" {
    fn cato_lld_link(
        flavor: i32,
        argv: *const *const c_char,
        argc: i32,
        out_err: *mut *mut c_char,
    ) -> i32;
    fn cato_lld_free_err(p: *mut c_char);
}

#[cfg(feature = "link")]
pub fn link(flavor: Flavor, argv: &[&CStr]) -> Result<(), String> {
    let flavor = match flavor {
        Flavor::Elf => 0i32,
        Flavor::Mingw => 1i32,
        Flavor::Darwin => 2i32,
    };
    let mut out_err: *mut c_char = std::ptr::null_mut();
    let mut cargv: Vec<*const c_char> = Vec::with_capacity(argv.len() + 1);
    let prog = CStr::from_bytes_with_nul(b"ld.lld\0").unwrap();
    cargv.push(prog.as_ptr());
    for a in argv {
        cargv.push(a.as_ptr());
    }
    let rc = unsafe {
        cato_lld_link(
            flavor,
            cargv.as_ptr(),
            cargv.len() as i32,
            &mut out_err as *mut *mut c_char,
        )
    };
    if rc == 0 {
        unsafe {
            if !out_err.is_null() {
                cato_lld_free_err(out_err);
            }
        }
        Ok(())
    } else {
        let err = if out_err.is_null() {
            "lld failed".to_string()
        } else {
            let s = unsafe { CStr::from_ptr(out_err) }.to_string_lossy().to_string();
            unsafe { cato_lld_free_err(out_err) };
            if s.is_empty() { "lld failed".to_string() } else { s }
        };
        Err(err)
    }
}

#[cfg(feature = "link")]
#[test]
fn shim_reports_no_input_files() {
    let e = link(Flavor::Elf, &[]).expect_err("empty argv must fail");
    assert!(e.to_lowercase().contains("no input"), "got: {e}");
}
