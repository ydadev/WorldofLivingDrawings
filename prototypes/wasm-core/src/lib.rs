#![no_std]

use core::panic::PanicInfo;

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}

/// Minimal shared arithmetic probe; this is not the simulation design.
#[unsafe(no_mangle)]
pub extern "C" fn advance_phase(phase: f32, delta: f32) -> f32 {
    let value = (phase + delta) % 1.0;
    if value < 0.0 { value + 1.0 } else { value }
}
