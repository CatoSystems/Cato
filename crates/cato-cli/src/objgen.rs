//! In-process LLVM IR -> object codegen (`static-llvm`).

use std::ffi::{c_char, CStr, CString};
use std::ptr;
use std::slice;

fn take_message(msg: *mut c_char) -> String {
    if msg.is_null() {
        return "unknown LLVM error".to_string();
    }
    let s = unsafe { CStr::from_ptr(msg) }
        .to_string_lossy()
        .into_owned();
    unsafe { llvm_sys::core::LLVMDisposeMessage(msg) };
    s
}

/// The hermetic triples only ever need the X86 and AArch64 backends; the
/// toolchain is built with exactly those. All-target initializers would
/// reference backends this build does not contain.
fn init_targets() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| unsafe {
        llvm_sys::target::LLVMInitializeX86TargetInfo();
        llvm_sys::target::LLVMInitializeX86Target();
        llvm_sys::target::LLVMInitializeX86TargetMC();
        llvm_sys::target::LLVMInitializeX86AsmPrinter();
        llvm_sys::target::LLVMInitializeAArch64TargetInfo();
        llvm_sys::target::LLVMInitializeAArch64Target();
        llvm_sys::target::LLVMInitializeAArch64TargetMC();
        llvm_sys::target::LLVMInitializeAArch64AsmPrinter();
    });
}

pub fn emit_object(ir: &str, llvm_triple: &str, opt_level: u8) -> Result<Vec<u8>, String> {
    use llvm_sys::target_machine::{
        LLVMCodeGenFileType, LLVMCodeGenOptLevel, LLVMCodeModel, LLVMRelocMode,
    };

    init_targets();
    let triple =
        CString::new(llvm_triple).map_err(|_| format!("invalid target triple '{llvm_triple}'"))?;
    let opt = match opt_level {
        0 => LLVMCodeGenOptLevel::LLVMCodeGenLevelNone,
        1 => LLVMCodeGenOptLevel::LLVMCodeGenLevelLess,
        2 => LLVMCodeGenOptLevel::LLVMCodeGenLevelDefault,
        _ => LLVMCodeGenOptLevel::LLVMCodeGenLevelAggressive,
    };
    let buffer_name = CString::new("cato").unwrap();
    let cpu = CString::new("").unwrap();
    let features = CString::new("").unwrap();

    unsafe {
        let ctx = llvm_sys::core::LLVMContextCreate();
        if ctx.is_null() {
            return Err("llvm: cannot create context".to_string());
        }
        let buf = llvm_sys::core::LLVMCreateMemoryBufferWithMemoryRangeCopy(
            ir.as_ptr() as *const c_char,
            ir.len(),
            buffer_name.as_ptr(),
        );
        if buf.is_null() {
            llvm_sys::core::LLVMContextDispose(ctx);
            return Err("llvm: cannot create memory buffer".to_string());
        }

        let mut module = ptr::null_mut();
        let mut msg: *mut c_char = ptr::null_mut();
        if llvm_sys::ir_reader::LLVMParseIRInContext2(ctx, buf, &mut module, &mut msg) != 0 {
            llvm_sys::core::LLVMDisposeMemoryBuffer(buf);
            let e = take_message(msg);
            llvm_sys::core::LLVMContextDispose(ctx);
            return Err(format!("llvm: cannot parse IR: {e}"));
        }
        llvm_sys::core::LLVMDisposeMemoryBuffer(buf);

        let mut target = ptr::null_mut();
        if llvm_sys::target_machine::LLVMGetTargetFromTriple(triple.as_ptr(), &mut target, &mut msg)
            != 0
        {
            let e = take_message(msg);
            llvm_sys::core::LLVMDisposeModule(module);
            llvm_sys::core::LLVMContextDispose(ctx);
            return Err(format!("llvm: unknown target triple '{llvm_triple}': {e}"));
        }

        let machine = llvm_sys::target_machine::LLVMCreateTargetMachine(
            target,
            triple.as_ptr(),
            cpu.as_ptr(),
            features.as_ptr(),
            opt,
            LLVMRelocMode::LLVMRelocDefault,
            LLVMCodeModel::LLVMCodeModelDefault,
        );
        if machine.is_null() {
            llvm_sys::core::LLVMDisposeModule(module);
            llvm_sys::core::LLVMContextDispose(ctx);
            return Err(format!(
                "llvm: cannot create target machine for '{llvm_triple}'"
            ));
        }

        let mut out_buf = ptr::null_mut();
        let status = llvm_sys::target_machine::LLVMTargetMachineEmitToMemoryBuffer(
            machine,
            module,
            LLVMCodeGenFileType::LLVMObjectFile,
            &mut msg,
            &mut out_buf,
        );
        let result = if status != 0 {
            Err(format!(
                "llvm: codegen failed for '{llvm_triple}': {}",
                take_message(msg)
            ))
        } else {
            let start = llvm_sys::core::LLVMGetBufferStart(out_buf) as *const u8;
            let size = llvm_sys::core::LLVMGetBufferSize(out_buf);
            Ok(slice::from_raw_parts(start, size).to_vec())
        };

        if !out_buf.is_null() {
            llvm_sys::core::LLVMDisposeMemoryBuffer(out_buf);
        }
        llvm_sys::target_machine::LLVMDisposeTargetMachine(machine);
        llvm_sys::core::LLVMDisposeModule(module);
        llvm_sys::core::LLVMContextDispose(ctx);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN_IR: &str = "define i32 @main() {\nentry:\n  ret i32 0\n}\n";

    #[test]
    fn emits_elf_for_musl_triples() {
        for (triple, machine) in [
            ("x86_64-unknown-linux-musl", 0x3Eu16),
            ("aarch64-unknown-linux-musl", 0xB7u16),
        ] {
            let obj = emit_object(MAIN_IR, triple, 2).unwrap();
            assert_eq!(&obj[..4], b"\x7fELF", "{triple}");
            let e_machine = u16::from_le_bytes([obj[18], obj[19]]);
            assert_eq!(e_machine, machine, "{triple}");
        }
    }

    #[test]
    fn emits_coff_for_windows_gnu() {
        let obj = emit_object(MAIN_IR, "x86_64-pc-windows-gnu", 2).unwrap();
        assert_eq!(u16::from_le_bytes([obj[0], obj[1]]), 0x8664);
    }

    #[test]
    fn emits_macho_for_darwin_triples() {
        for triple in ["x86_64-apple-darwin", "aarch64-apple-darwin"] {
            let obj = emit_object(MAIN_IR, triple, 2).unwrap();
            let magic = u32::from_le_bytes([obj[0], obj[1], obj[2], obj[3]]);
            assert!(
                magic == 0xfeedfacf || magic == 0xcffaedfe,
                "{triple}: {magic:#x}"
            );
        }
    }

    #[test]
    fn garbage_ir_is_an_error() {
        assert!(emit_object("this is not ir", "x86_64-unknown-linux-musl", 2).is_err());
    }
}
