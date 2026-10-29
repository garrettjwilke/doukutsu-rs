use std::any::Any;
use std::cell::RefCell;
use std::ffi::CStr;
use std::os::raw::c_char;
use std::path::PathBuf;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use crate::data::builtin_fs::BuiltinFS;
use crate::framework::backend::{AudioEvent, BackendAudio, BackendCallbacks, WindowParams};
use crate::framework::context::Context;
use crate::framework::error::GameResult;
use crate::framework::filesystem::{mount_user_vfs, mount_vfs};
use crate::framework::keyboard::ScanCode;
use crate::framework::render::soft_impl::SoftRenderer;
use crate::framework::vfs::PhysicalFS;
use crate::game::filesystem_container::FilesystemContainer;
use crate::game::scripting::tsc::text_script::ScriptMode;
use crate::game::settings::VSyncMode;
use crate::game::shared_game_state::{TimingMode, WindowMode};
use crate::game::Game;
use crate::scene::game_scene::GameScene;
use crate::scene::loading_scene::LoadingScene;

pub const CS_BTN_LEFT: u16 = 1 << 0;
pub const CS_BTN_DOWN: u16 = 1 << 1;
pub const CS_BTN_RIGHT: u16 = 1 << 2;
pub const CS_BTN_UP: u16 = 1 << 3;
pub const CS_BTN_JUMP: u16 = 1 << 4;
pub const CS_BTN_SHOOT: u16 = 1 << 5;
pub const CS_BTN_PREV_WPN: u16 = 1 << 6;
pub const CS_BTN_NEXT_WPN: u16 = 1 << 7;
pub const CS_BTN_INVENTORY: u16 = 1 << 8;
pub const CS_BTN_MAP: u16 = 1 << 9;
pub const CS_BTN_STRAFE: u16 = 1 << 10;
pub const CS_BTN_SKIP: u16 = 1 << 11;
pub const CS_BTN_PAUSE: u16 = 1 << 12;
pub const CS_BTN_OK: u16 = 1 << 13;

#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct CsInputState {
    pub p1_buttons: u16,
    pub p2_buttons: u16,
}

type AudioCallbackHolder = Arc<Mutex<Option<Box<dyn FnMut(&mut [f32]) + Send>>>>;

struct MpcAudio {
    callback: AudioCallbackHolder,
    sample_rate: u32,
    paused: bool,
}

impl MpcAudio {
    fn new(callback: AudioCallbackHolder, sample_rate: u32) -> Self {
        Self {
            callback,
            sample_rate,
            paused: false,
        }
    }
}

impl BackendAudio for MpcAudio {
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn channels(&self) -> u16 {
        2
    }

    fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    fn update(&mut self) -> Vec<AudioEvent> {
        Vec::new()
    }

    fn set_callback(&mut self, callback: Box<dyn FnMut(&mut [f32]) + Send>) {
        if let Ok(mut guard) = self.callback.lock() {
            *guard = Some(callback);
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub struct CsRunner {
    ctx: Pin<Box<Context>>,
    game: Pin<Box<Game>>,
    screen_buf: Rc<RefCell<Vec<u8>>>,
    audio_cb: AudioCallbackHolder,
    audio_scratch: Mutex<Vec<f32>>,
    data_dir: PathBuf,
    user_dir: PathBuf,
    samplerate: u32,
    is_paused: bool,
    frame_count: u64,
}

impl CsRunner {
    fn new(data_dir: PathBuf, user_dir: PathBuf, samplerate: u32) -> GameResult<Self> {
        let _ = std::fs::create_dir_all(&user_dir);
        if !data_dir.as_os_str().is_empty() && data_dir.is_dir() {
            std::env::set_var("CAVESTORY_DATA_DIR", &data_dir);
        }

        let mut ctx = Box::pin(Context::new());

        if !data_dir.as_os_str().is_empty() && data_dir.is_dir() {
            mount_vfs(&mut ctx, Box::new(PhysicalFS::new(&data_dir, true)));
        }
        mount_vfs(&mut ctx, Box::new(BuiltinFS::new_embedded_gamedata()));
        mount_user_vfs(&mut ctx, Box::new(PhysicalFS::new(&user_dir, false)));
        mount_vfs(&mut ctx, Box::new(BuiltinFS::new()));

        let soft_renderer = SoftRenderer::new(320, 240);
        let screen_buf = soft_renderer.screen_buf.clone();
        ctx.renderer = Some(Box::new(soft_renderer));

        let audio_cb: AudioCallbackHolder = Arc::new(Mutex::new(None));
        ctx.audio = Some(Box::new(MpcAudio::new(audio_cb.clone(), samplerate)));

        ctx.window = WindowParams {
            size_hint: (320, 240),
            mode: WindowMode::Fullscreen,
        };
        ctx.viewport.window_size = (320, 240);
        ctx.viewport.recompute();

        let mut game = Box::pin(Game::new(&mut ctx)?);
        let fs_container = FilesystemContainer {
            user_path: user_dir.clone(),
            game_path: data_dir.clone(),
            is_portable: true,
        };

        {
            let g = game.as_mut().get_mut();
            let state = g.state.get_mut();
            state.fs_container = Some(fs_container);
            state.settings.vsync_mode = VSyncMode::Uncapped;
            state.settings.touch_controls = false;
            state.settings.subpixel_coords = false;
            state.settings.motion_interpolation = false;
            if let Some(audio) = &mut ctx.audio {
                state.sound_manager.attach_audio(audio);
            }
        }

        game.on_resize(&mut ctx)?;
        game.as_mut().get_mut().state.get_mut().next_scene = Some(Box::new(LoadingScene::new()));

        let mut runner = Self {
            ctx,
            game,
            screen_buf,
            audio_cb,
            audio_scratch: Mutex::new(Vec::with_capacity(4096)),
            data_dir,
            user_dir,
            samplerate,
            is_paused: false,
            frame_count: 0,
        };

        // Run initial tick to kick off LoadingScene
        let empty = CsInputState::default();
        runner.tick(&empty);

        Ok(runner)
    }

    fn apply_input(&mut self, input: &CsInputState) {
        let (p1_map, p2_map) = {
            let s = self.game.as_mut().get_mut().state.get_mut();
            (
                (
                    s.settings.player1_key_map.left,
                    s.settings.player1_key_map.down,
                    s.settings.player1_key_map.right,
                    s.settings.player1_key_map.up,
                    s.settings.player1_key_map.jump,
                    s.settings.player1_key_map.shoot,
                    s.settings.player1_key_map.prev_weapon,
                    s.settings.player1_key_map.next_weapon,
                    s.settings.player1_key_map.inventory,
                    s.settings.player1_key_map.map,
                    s.settings.player1_key_map.strafe,
                    s.settings.player1_key_map.skip,
                    s.settings.player1_key_map.menu_ok,
                    s.settings.player1_key_map.menu_back,
                ),
                (
                    s.settings.player2_key_map.left,
                    s.settings.player2_key_map.down,
                    s.settings.player2_key_map.right,
                    s.settings.player2_key_map.up,
                    s.settings.player2_key_map.jump,
                    s.settings.player2_key_map.shoot,
                    s.settings.player2_key_map.prev_weapon,
                    s.settings.player2_key_map.next_weapon,
                    s.settings.player2_key_map.inventory,
                    s.settings.player2_key_map.map,
                    s.settings.player2_key_map.strafe,
                    s.settings.player2_key_map.skip,
                    s.settings.player2_key_map.menu_ok,
                    s.settings.player2_key_map.menu_back,
                ),
            )
        };

        let kb = &mut self.ctx.keyboard_context;

        // Clear all mapped keys first so overlapping bindings (e.g. skip/inventory = Q) don't clobber each other
        for code in [
            p1_map.0, p1_map.1, p1_map.2, p1_map.3, p1_map.4, p1_map.5, p1_map.6, p1_map.7,
            p1_map.8, p1_map.9, p1_map.10, p1_map.11, p1_map.12, p1_map.13,
            p2_map.0, p2_map.1, p2_map.2, p2_map.3, p2_map.4, p2_map.5, p2_map.6, p2_map.7,
            p2_map.8, p2_map.9, p2_map.10, p2_map.11, p2_map.12, p2_map.13,
            ScanCode::Escape,
            ScanCode::Return,
        ] {
            kb.set_key(code, false);
        }

        let p1 = input.p1_buttons;
        if (p1 & CS_BTN_LEFT) != 0 { kb.set_key(p1_map.0, true); }
        if (p1 & CS_BTN_DOWN) != 0 { kb.set_key(p1_map.1, true); }
        if (p1 & CS_BTN_RIGHT) != 0 { kb.set_key(p1_map.2, true); }
        if (p1 & CS_BTN_UP) != 0 { kb.set_key(p1_map.3, true); }
        if (p1 & CS_BTN_JUMP) != 0 {
            kb.set_key(p1_map.4, true);
            kb.set_key(p1_map.12, true);
        }
        if (p1 & CS_BTN_SHOOT) != 0 {
            kb.set_key(p1_map.5, true);
            kb.set_key(p1_map.13, true);
        }
        if (p1 & CS_BTN_PREV_WPN) != 0 { kb.set_key(p1_map.6, true); }
        if (p1 & CS_BTN_NEXT_WPN) != 0 { kb.set_key(p1_map.7, true); }
        if (p1 & CS_BTN_INVENTORY) != 0 { kb.set_key(p1_map.8, true); }
        if (p1 & CS_BTN_MAP) != 0 { kb.set_key(p1_map.9, true); }
        if (p1 & CS_BTN_STRAFE) != 0 { kb.set_key(p1_map.10, true); }
        if (p1 & CS_BTN_SKIP) != 0 { kb.set_key(p1_map.11, true); }
        if (p1 & CS_BTN_PAUSE) != 0 { kb.set_key(ScanCode::Escape, true); }
        if (p1 & CS_BTN_OK) != 0 {
            kb.set_key(ScanCode::Return, true);
            kb.set_key(p1_map.12, true);
        }

        let p2 = input.p2_buttons;
        if (p2 & CS_BTN_LEFT) != 0 { kb.set_key(p2_map.0, true); }
        if (p2 & CS_BTN_DOWN) != 0 { kb.set_key(p2_map.1, true); }
        if (p2 & CS_BTN_RIGHT) != 0 { kb.set_key(p2_map.2, true); }
        if (p2 & CS_BTN_UP) != 0 { kb.set_key(p2_map.3, true); }
        if (p2 & CS_BTN_JUMP) != 0 {
            kb.set_key(p2_map.4, true);
            kb.set_key(p2_map.12, true);
        }
        if (p2 & CS_BTN_SHOOT) != 0 {
            kb.set_key(p2_map.5, true);
            kb.set_key(p2_map.13, true);
        }
        if (p2 & CS_BTN_PREV_WPN) != 0 { kb.set_key(p2_map.6, true); }
        if (p2 & CS_BTN_NEXT_WPN) != 0 { kb.set_key(p2_map.7, true); }
        if (p2 & CS_BTN_INVENTORY) != 0 { kb.set_key(p2_map.8, true); }
        if (p2 & CS_BTN_MAP) != 0 { kb.set_key(p2_map.9, true); }
        if (p2 & CS_BTN_STRAFE) != 0 { kb.set_key(p2_map.10, true); }
        if (p2 & CS_BTN_SKIP) != 0 { kb.set_key(p2_map.11, true); }
        if (p2 & CS_BTN_PAUSE) != 0 { kb.set_key(ScanCode::Escape, true); }
        if (p2 & CS_BTN_OK) != 0 {
            kb.set_key(ScanCode::Return, true);
            kb.set_key(p2_map.12, true);
        }
    }

    fn tick(&mut self, input: &CsInputState) {
        if self.is_paused {
            return;
        }

        self.apply_input(input);

        let game = self.game.as_mut().get_mut();
        if let Some(audio) = &mut self.ctx.audio {
            game.state.get_mut().sound_manager.attach_audio(audio);
        }

        let fast_forward = {
            let s = game.state.get_mut();
            s.textscript_vm.mode == ScriptMode::Map && s.textscript_vm.flags.cutscene_skip()
        };
        let loops: u32 = if fast_forward { 4 } else { 1 };
        game.loops = loops;

        if let Some(scene) = game.scene.get_mut() {
            let state_ref = game.state.get_mut();
            let _ = scene.draw_tick(state_ref);
            for _ in 0..loops {
                let _ = scene.tick(state_ref, &mut self.ctx);
            }
        }

        let next_scene = std::mem::take(&mut game.state.get_mut().next_scene);
        if let Some(mut next_scene) = next_scene {
            {
                let state_ref = game.state.get_mut();
                let _ = next_scene.init(state_ref, &mut self.ctx);
                state_ref.frame_time = 0.0;
            }
            *game.scene.get_mut() = Some(next_scene);
            game.loops = 0;
        }

        self.frame_count = self.frame_count.wrapping_add(1);
    }

    fn render(&mut self, dst_fb_320x240: &mut [u32]) {
        let _ = self.game.as_mut().get_mut().draw(&mut self.ctx);
        let rgba = self.screen_buf.borrow();
        let count = dst_fb_320x240.len().min(320 * 240);
        for (i, dst_px) in dst_fb_320x240.iter_mut().take(count).enumerate() {
            let off = i * 4;
            let r = rgba[off] as u32;
            let g = rgba[off + 1] as u32;
            let b = rgba[off + 2] as u32;
            *dst_px = (r << 16) | (g << 8) | b;
        }
    }

    fn fill_audio(&self, dst_stereo: &mut [i16], frames: usize) {
        if self.is_paused {
            dst_stereo.fill(0);
            return;
        }
        let samples = frames * 2;
        if let Ok(mut cb_guard) = self.audio_cb.lock() {
            if let Some(cb) = cb_guard.as_mut() {
                if let Ok(mut scratch) = self.audio_scratch.lock() {
                    if scratch.len() < samples {
                        scratch.resize(samples, 0.0);
                    }
                    let slice = &mut scratch[..samples];
                    slice.fill(0.0);
                    cb(slice);
                    for i in 0..samples {
                        let s = (slice[i].clamp(-1.0, 1.0) * 32767.0) as i16;
                        dst_stereo[i] = s;
                    }
                    return;
                }
            }
        }
        dst_stereo.fill(0);
    }

    fn is_in_menu(&mut self) -> bool {
        let game = self.game.as_mut().get_mut();
        let mode = game.state.get_mut().textscript_vm.mode;
        if mode == ScriptMode::Inventory || mode == ScriptMode::StageSelect {
            return true;
        }
        if let Some(scene) = game.scene.get_mut() {
            if let Ok(gs) = downcast::Downcast::<GameScene>::downcast_ref(scene.as_ref()) {
                return gs.pause_menu.is_paused();
            }
            return true;
        }
        true
    }
}

#[no_mangle]
pub unsafe extern "C" fn cs_runner_create(
    data_dir: *const c_char,
    user_dir: *const c_char,
    samplerate: i32,
) -> *mut CsRunner {
    if user_dir.is_null() {
        return std::ptr::null_mut();
    }
    let data_path = if data_dir.is_null() {
        PathBuf::new()
    } else {
        PathBuf::from(CStr::from_ptr(data_dir).to_string_lossy().into_owned())
    };
    let user_path = PathBuf::from(CStr::from_ptr(user_dir).to_string_lossy().into_owned());
    let sr = if samplerate > 0 { samplerate as u32 } else { 44100 };

    match CsRunner::new(data_path, user_path, sr) {
        Ok(runner) => Box::into_raw(Box::new(runner)),
        Err(e) => {
            eprintln!("[cs_runner] Failed to create runner: {:?}", e);
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn cs_runner_destroy(r: *mut CsRunner) {
    if !r.is_null() {
        drop(Box::from_raw(r));
    }
}

#[no_mangle]
pub unsafe extern "C" fn cs_runner_reset(r: *mut CsRunner) {
    if r.is_null() {
        return;
    }
    let runner = &mut *r;
    let data_dir = runner.data_dir.clone();
    let user_dir = runner.user_dir.clone();
    let sr = runner.samplerate;
    if let Ok(new_runner) = CsRunner::new(data_dir, user_dir, sr) {
        *runner = new_runner;
    }
}

#[no_mangle]
pub unsafe extern "C" fn cs_runner_tick(r: *mut CsRunner, input: *const CsInputState) {
    if r.is_null() || input.is_null() {
        return;
    }
    let runner = &mut *r;
    runner.tick(&*input);
}

#[no_mangle]
pub unsafe extern "C" fn cs_runner_render(r: *mut CsRunner, dst_fb_320x240: *mut u32) {
    if r.is_null() || dst_fb_320x240.is_null() {
        return;
    }
    let runner = &mut *r;
    let slice = std::slice::from_raw_parts_mut(dst_fb_320x240, 320 * 240);
    runner.render(slice);
}

#[no_mangle]
pub unsafe extern "C" fn cs_runner_fill_audio(r: *mut CsRunner, dst_stereo: *mut i16, frames: i32) {
    if r.is_null() || dst_stereo.is_null() || frames <= 0 {
        return;
    }
    let runner = &*r;
    let slice = std::slice::from_raw_parts_mut(dst_stereo, (frames as usize) * 2);
    runner.fill_audio(slice, frames as usize);
}

#[no_mangle]
pub unsafe extern "C" fn cs_runner_is_paused(r: *const CsRunner) -> bool {
    if r.is_null() {
        return false;
    }
    (*r).is_paused
}

#[no_mangle]
pub unsafe extern "C" fn cs_runner_set_paused(r: *mut CsRunner, paused: bool) {
    if r.is_null() {
        return;
    }
    (*r).is_paused = paused;
}

#[no_mangle]
pub unsafe extern "C" fn cs_runner_is_in_menu(r: *mut CsRunner) -> bool {
    if r.is_null() {
        return false;
    }
    (*r).is_in_menu()
}

#[no_mangle]
pub unsafe extern "C" fn cs_runner_should_quit(r: *mut CsRunner) -> bool {
    if r.is_null() {
        return false;
    }
    (&(*r).ctx).shutdown_requested
}

#[no_mangle]
pub unsafe extern "C" fn cs_runner_get_tps(r: *mut CsRunner) -> i32 {
    if r.is_null() {
        return 50;
    }
    let runner = &mut *r;
    match runner.game.as_mut().get_mut().state.get_mut().settings.timing_mode {
        TimingMode::_60Hz => 60,
        _ => 50,
    }
}

#[no_mangle]
pub unsafe extern "C" fn cs_runner_get_frame_count(r: *const CsRunner) -> u64 {
    if r.is_null() {
        return 0;
    }
    (*r).frame_count
}
