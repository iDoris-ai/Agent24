//! Hardware probing moved to the `agent24-hw` crate (HW-1, ADR-036: one
//! machine, one prober). This module re-exports it so the
//! `agent24_decide::{Accelerator, HardwareProbe, HardwareProfile,
//! SystemProbe, FakeHardwareProbe}` paths keep working.

pub use agent24_hw::{
    Accelerator, Budget, DiskInfo, FakeHardwareProbe, GpuInfo, HardwareProbe, HardwareProfile,
    HwLevel, SystemProbe,
};
