#![no_std]
#![no_main]

#[cfg(feature = "owned")]
#[global_allocator]
static ALLOCATOR: dlmalloc::GlobalDlmalloc = dlmalloc::GlobalDlmalloc;

#[no_mangle]
pub extern "C" fn exercise() -> bool {
    object_id_portability_consumer::exercise().is_ok()
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {}
}
