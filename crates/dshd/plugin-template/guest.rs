// Trusted canonical-ABI wrapper for dsh:plugin@1.0.0; compiled without Cargo.
// Only the function body placeholder is generated. The compiler is sandboxed.
use std::alloc::{alloc, dealloc, realloc, Layout};

fn plugin(input: &str) -> String {
    /* DSH_GENERATED_BODY */
}

#[export_name = "health"]
pub extern "C" fn health() -> i32 { 1 }

#[export_name = "invoke"]
pub unsafe extern "C" fn invoke(ptr: *mut u8, len: usize) -> *mut usize {
    let input = String::from_raw_parts(ptr, len, len);
    let result = plugin(&input).into_bytes().into_boxed_slice();
    let result_len = result.len();
    let result_ptr = Box::into_raw(result) as *mut u8;
    Box::into_raw(Box::new([result_ptr as usize, result_len])) as *mut usize
}

#[export_name = "cabi_post_invoke"]
pub unsafe extern "C" fn post_invoke(ret: *mut usize) {
    let record = Box::from_raw(ret as *mut [usize; 2]);
    drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(record[0] as *mut u8, record[1])));
}

#[export_name = "cabi_realloc"]
pub unsafe extern "C" fn cabi_realloc(ptr: *mut u8, old: usize, align: usize, new: usize) -> *mut u8 {
    if new == 0 {
        if old != 0 { dealloc(ptr, Layout::from_size_align_unchecked(old, align)); }
        return align as *mut u8;
    }
    let result = if old == 0 {
        alloc(Layout::from_size_align_unchecked(new, align))
    } else {
        realloc(ptr, Layout::from_size_align_unchecked(old, align), new)
    };
    if result.is_null() { std::process::abort(); }
    result
}
