use std::{ffi::CString, fmt::Display, time::Instant};

use thiserror::Error;
use windows::{
    Win32::{
        Foundation::TRUE,
        Graphics::{
            DirectComposition::{
                COMPOSITION_FRAME_ID_COMPLETED, COMPOSITION_FRAME_STATS, COMPOSITION_TARGET_ID,
                COMPOSITION_TARGET_STATS, DCompositionGetFrameId, DCompositionGetStatistics,
                DCompositionGetTargetStatistics,
            },
            Dxgi::{
                Common::DXGI_FORMAT_R8G8B8A8_UNORM, CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS,
                DXGI_ENUM_MODES_SCALING, DXGI_MODE_DESC1, IDXGIFactory7, IDXGIOutput6,
            },
            Gdi::{DEVMODEA, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsA},
        },
        System::WindowsProgramming::QueryInterruptTime,
        UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
    },
    core::{Interface as _, PCSTR},
};

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

pub struct Win11TimeDriver {}

impl Win11TimeDriver {
    pub fn new() -> Result<Self, Win11TimeDriverError> {
        unsafe {
            SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
                .map_err(Win11TimeDriverError::DpiAwareness)?;

            let factory: IDXGIFactory7 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS::default())
                .map_err(Win11TimeDriverError::CreateDXGIFactory)?;

            let mut total_outputs = 0u32;

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
                    }

                    std::thread::spawn(move || {
                        let mut last_time = Instant::now();
                        loop {
                            output6.WaitForVBlank().unwrap();

                            let now = Instant::now();
                            let diff = now.duration_since(last_time);
                            last_time = now;
                            println!("VBlank {}: {:?}", output_idx, diff);
                        }
                    });

                    output_idx += 1;
                    total_outputs += 1;
                }
                adapter_idx += 1;
            }

            let _timings = DCompositionTimings::new(total_outputs)?;
        }

        Ok(Self {})
    }
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

struct DCompositionTimings {
    frame_stats: COMPOSITION_FRAME_STATS,
    target_stats: Vec<COMPOSITION_TARGET_STATS>,
}

impl DCompositionTimings {
    fn new(count: u32) -> Result<Self, Win11TimeDriverError> {
        unsafe {
            let now = Instant::now();
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

            let qtp_now = QueryInterruptTime();
            let int_now = current_interrupt_time();

            let mut target_stats = Vec::new();
            for target_id in &target_ids {
                target_stats.push(
                    DCompositionGetTargetStatistics(composition_frame_id, target_id).unwrap(),
                );
            }

            let elapsed = now.elapsed();
            println!("DCompositionTimings::new took {:?}", elapsed);

            dbg!(qtp_now, frame_stats);

            let start_time = duration_from_interrupt_time(frame_stats.startTime);
            let target_time = duration_from_interrupt_time(frame_stats.targetTime);

            println!(
                "Frame Stats: Start: {} - Target: {}",
                ComparisonTime::new(int_now, start_time),
                ComparisonTime::new(int_now, target_time)
            );

            for (i, stat) in target_stats.iter_mut().enumerate() {
                let last_present = duration_from_interrupt_time(stat.presentTime);
                let vblank_duration = duration_from_interrupt_time(stat.vblankDuration);
                let last_presented_time = duration_from_interrupt_time(stat.presentedStats.time);

                let last_present_cmp = ComparisonTime::new(int_now, last_present);
                let last_presented_cmp = ComparisonTime::new(int_now, last_presented_time);

                println!(
                    "{i} VBlank Duration: {:?}, Last Present: {} - Last Presented: {}",
                    vblank_duration, last_present_cmp, last_presented_cmp
                );
            }

            Ok(Self {
                frame_stats,
                target_stats,
            })
        }
    }
}

fn string_from_utf16_nul(data: &[u16]) -> String {
    let len = data.iter().position(|&c| c == 0).unwrap_or(data.len());
    String::from_utf16_lossy(&data[..len])
}

fn duration_from_interrupt_time(ticks: u64) -> std::time::Duration {
    std::time::Duration::from_nanos(ticks * 100)
}

fn current_interrupt_time() -> std::time::Duration {
    duration_from_interrupt_time(unsafe { QueryInterruptTime() })
}

struct ComparisonTime {
    now: std::time::Duration,
    time: std::time::Duration,
}

impl ComparisonTime {
    fn new(now: std::time::Duration, time: std::time::Duration) -> Self {
        Self { now, time }
    }
}

impl Display for ComparisonTime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.now > self.time {
            let diff = self.now - self.time;
            write!(f, "{:?} ago", diff)
        } else {
            let diff = self.time - self.now;
            write!(f, "in {:?}", diff)
        }
    }
}
