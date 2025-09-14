use std::{
    ffi::CString,
    fmt::Display,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use thiserror::Error;
use windows::{
    Win32::{
        Foundation::{HANDLE, TRUE, WAIT_OBJECT_0},
        Graphics::{
            DirectComposition::{
                COMPOSITION_FRAME_ID_COMPLETED, COMPOSITION_FRAME_STATS, COMPOSITION_TARGET_ID,
                COMPOSITION_TARGET_STATS, DCompositionGetFrameId, DCompositionGetStatistics,
                DCompositionGetTargetStatistics, DCompositionWaitForCompositorClock,
            },
            Dxgi::{
                Common::DXGI_FORMAT_R8G8B8A8_UNORM, CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS,
                DXGI_ENUM_MODES_SCALING, DXGI_MODE_DESC1, DXGI_OUTPUT_DESC1, IDXGIFactory7,
                IDXGIOutput6,
            },
            Gdi::{DEVMODEA, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsA},
        },
        System::{
            Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
            Threading::CreateEventA,
        },
        UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
    },
    core::{Interface as _, PCSTR},
};

use crate::drivers::TimeDriver;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Win11TimeDriverError {
    #[error("Error inside SetProcessDpiAwarenessContext")]
    DpiAwareness(#[source] windows::core::Error),

    #[error("Error creating DXGI factory")]
    CreateDXGIFactory(#[source] windows::core::Error),
    #[error("Error inside EnumDisplaySettingsA")]
    EnumDisplaySettings(#[source] windows::core::Error),
    #[error("Error inside GetDisplayModeList1")]
    EnumDisplayModeList(#[source] windows::core::Error),
    #[error("Failed to find a matching display mode")]
    NoMatchingDisplayMode,
}

pub struct Win11TimeDriver {
    outputs: Vec<Output>,
    frame_stats: Arc<Mutex<DCompositionTimings>>,
    stop_event: HANDLE,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Win11TimeDriver {
    pub fn new() -> Result<Self, Win11TimeDriverError> {
        unsafe {
            let factory: IDXGIFactory7 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS::default())
                .map_err(Win11TimeDriverError::CreateDXGIFactory)?;

            let mut outputs = Vec::new();

            let mut adapter_idx = 0;
            while let Ok(adapter) = factory.EnumAdapters1(adapter_idx) {
                let adapter_desc = adapter.GetDesc1().unwrap();

                let adapter_name = string_from_utf16_nul(&adapter_desc.Description);

                let mut output_idx = 0;
                while let Ok(output) = adapter.EnumOutputs(output_idx) {
                    let output6 = output.cast::<IDXGIOutput6>().unwrap();

                    if let Ok(output_desc) = output6.GetDesc1() {
                        let name = string_from_utf16_nul(&output_desc.DeviceName);

                        let found_mode = get_active_mode(&output6, &name)?;

                        println!(
                            "Display {} on {}: {} ({}x{}): {:?} Hz",
                            output_idx,
                            adapter_name,
                            name,
                            output_desc.DesktopCoordinates.right
                                - output_desc.DesktopCoordinates.left,
                            output_desc.DesktopCoordinates.bottom
                                - output_desc.DesktopCoordinates.top,
                            found_mode.RefreshRate.Numerator as f32
                                / found_mode.RefreshRate.Denominator as f32
                        );

                        outputs.push(Output {
                            inner: output6,
                            desc: output_desc,
                            mode: found_mode,
                        });
                    }

                    output_idx += 1;
                }
                adapter_idx += 1;
            }

            let timings = DCompositionTimings::new(outputs.len() as u32).unwrap();

            let frame_stats = Arc::new(Mutex::new(timings));

            let stop_event = CreateEventA(None, true, false, None).unwrap();

            let thread = {
                let frame_stats = frame_stats.clone();
                let stop_event = SendSyncWrapper(stop_event);
                let count = outputs.len() as u32;

                std::thread::spawn(move || {
                    // We need to bind the send_sync wrapper explicitly, otherwise only
                    // stop_event.0 will get moved in, and that is not Send + Sync.
                    let s = stop_event;
                    Self::thread(frame_stats, s.0, count);
                })
            };

            Ok(Self {
                outputs,
                stop_event,
                thread: Some(thread),
                frame_stats,
            })
        }
    }

    fn thread(frame_stats: Arc<Mutex<DCompositionTimings>>, stop_event: HANDLE, count: u32) {
        loop {
            let res = unsafe { DCompositionWaitForCompositorClock(Some(&[stop_event]), 100) };

            if res == WAIT_OBJECT_0.0 {
                println!("Stopping timing thread");
                // Our stop event was signaled.
                break;
            }

            if res != WAIT_OBJECT_0.0 + 1 {
                println!("Error occurred");
                // Some error occurred.
                break;
            }

            if let Ok(new_stats) = DCompositionTimings::new(count) {
                *frame_stats.lock().unwrap() = new_stats;
            }
        }
    }
}

impl TimeDriver for Win11TimeDriver {
    fn next_presentation(&self) -> super::FuturePresentation {
        let timings = self.frame_stats.lock().unwrap().clone();

        // TODO: Figure out which screen/compositor to use. For now using the zeroth monitor.
        let target_stats = &timings.target_stats[0];
        let output = &self.outputs[0];

        let now = Duration::now_qpc();
        let last_vsync = Duration::from_qpc(target_stats.presentedStats.time);
        let vsync_duration = std::time::Duration::from_nanos(
            ((1_000_000_000u128 * output.mode.RefreshRate.Denominator as u128)
                / output.mode.RefreshRate.Numerator as u128) as u64,
        );

        let intervals_behind = (now - last_vsync)
            .as_nanos()
            .div_ceil(vsync_duration.as_nanos());
        let next_present = last_vsync + vsync_duration * (intervals_behind as u32);

        super::FuturePresentation {
            now,
            soonest_presentation: next_present,
            latest_presentation: next_present,
            display_interval: super::DisplayInterval {
                interval: vsync_duration,
                vrr_range: None,
            },
        }
    }
}

impl Drop for Win11TimeDriver {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::System::Threading::SetEvent(self.stop_event);
        }
        let _ = self.thread.take().unwrap().join();
    }
}

struct Output {
    inner: IDXGIOutput6,
    desc: DXGI_OUTPUT_DESC1,
    mode: DXGI_MODE_DESC1,
}

fn get_active_mode(
    output6: &IDXGIOutput6,
    name: &str,
) -> Result<DXGI_MODE_DESC1, Win11TimeDriverError> {
    unsafe {
        let cname = CString::new(name).unwrap();
        let mut dev_mode = DEVMODEA::default();
        {
            let res = EnumDisplaySettingsA(
                PCSTR(cname.as_ptr() as *const u8),
                ENUM_CURRENT_SETTINGS,
                &mut dev_mode,
            );

            if res != TRUE {
                return Err(Win11TimeDriverError::EnumDisplaySettings(
                    windows::core::Error::from_thread(),
                ));
            }
        }
        let mut num_modes = 0;

        output6
            .GetDisplayModeList1(
                DXGI_FORMAT_R8G8B8A8_UNORM,
                DXGI_ENUM_MODES_SCALING,
                &mut num_modes,
                None,
            )
            .unwrap();
        let mut modes = vec![DXGI_MODE_DESC1::default(); num_modes as usize];
        output6
            .GetDisplayModeList1(
                DXGI_FORMAT_R8G8B8A8_UNORM,
                DXGI_ENUM_MODES_SCALING,
                &mut num_modes,
                Some(modes.as_mut_ptr()),
            )
            .unwrap();
        let mut found_mode = None;
        for mode in &modes {
            let freq = mode.RefreshRate.Numerator as f32 / mode.RefreshRate.Denominator as f32;
            if mode.Width == dev_mode.dmPelsWidth as u32
                && mode.Height == dev_mode.dmPelsHeight as u32
                && (freq - dev_mode.dmDisplayFrequency as f32).abs() < 1.0
            {
                found_mode = Some(mode);
                break;
            }
        }
        let found_mode = found_mode
            .ok_or(Win11TimeDriverError::NoMatchingDisplayMode)?
            .clone();
        Ok(found_mode)
    }
}

#[derive(Debug, Clone)]
struct DCompositionTimings {
    frame_stats: COMPOSITION_FRAME_STATS,
    target_stats: Vec<COMPOSITION_TARGET_STATS>,
}

impl DCompositionTimings {
    fn new(count: u32) -> Result<Self, Win11TimeDriverError> {
        unsafe {
            let composition_frame_id =
                DCompositionGetFrameId(COMPOSITION_FRAME_ID_COMPLETED).unwrap();

            let mut frame_stats = COMPOSITION_FRAME_STATS::default();
            let mut target_ids = vec![COMPOSITION_TARGET_ID::default(); count as usize];
            let mut target_id_count = count;

            DCompositionGetStatistics(
                composition_frame_id,
                &mut frame_stats,
                target_ids.len() as _,
                Some(target_ids.as_mut_ptr()),
                Some(&mut target_id_count),
            )
            .unwrap();

            assert_eq!(target_id_count, count);

            let mut target_stats = Vec::new();
            for target_id in &target_ids {
                target_stats.push(
                    DCompositionGetTargetStatistics(composition_frame_id, target_id).unwrap(),
                );
            }

            Ok(Self {
                frame_stats,
                target_stats,
            })
        }
    }
}

struct SendSyncWrapper<T>(T);

unsafe impl<T> Send for SendSyncWrapper<T> {}
unsafe impl<T> Sync for SendSyncWrapper<T> {}

fn string_from_utf16_nul(data: &[u16]) -> String {
    let len = data.iter().position(|&c| c == 0).unwrap_or(data.len());
    String::from_utf16_lossy(&data[..len])
}

static QPC_FREQUENCY: OnceLock<u64> = OnceLock::new();

fn get_frequency() -> u64 {
    let freq = *QPC_FREQUENCY.get_or_init(|| {
        let mut freq = 0;
        unsafe { QueryPerformanceFrequency(&mut freq).unwrap() };
        freq as u64
    });

    freq
}

pub trait DurationExt {
    fn now_qpc() -> Self;
    fn from_qpc(ticks: u64) -> Self;
    fn to_qpc(&self) -> u64;
}

impl DurationExt for std::time::Duration {
    fn now_qpc() -> Self {
        let mut count = 0;
        unsafe { QueryPerformanceCounter(&mut count).unwrap() };

        Self::from_qpc(count as u64)
    }

    fn from_qpc(ticks: u64) -> Self {
        let frequency = get_frequency();

        if frequency == 10_000_000 {
            // 100ns intervals, can convert directly to Duration
            std::time::Duration::from_nanos(ticks * 100)
        } else {
            // Convert to nanoseconds first
            let nanos = (ticks as u128 * 1_000_000_000u128) / (frequency as u128);
            let seconds = (nanos / 1_000_000_000) as u64;
            let nanos = (nanos % 1_000_000_000) as u32;
            std::time::Duration::new(seconds, nanos)
        }
    }

    fn to_qpc(&self) -> u64 {
        self.as_nanos() as u64 / 100
    }
}
