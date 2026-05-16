use std::thread;
use std::thread::JoinHandle;
use bitflags::{bitflags, bitflags_match};
use color_eyre::eyre;
use evdevil::{ff, AbsInfo, Bus, InputId};
use evdevil::event::{Abs, AbsEvent, EventKind, ForceFeedbackCode, Key, KeyEvent, KeyState, Misc, MiscEvent, UinputCode};
use evdevil::ff::EffectKind;
use evdevil::uinput::{AbsSetup, UinputDevice};
use crate::{RumbleEffect, SteamControllerHid};
use crate::virtual_controllers::dualsense::{DualsenseButtons, VirtualDualSenseController};

pub mod dualsense;

pub struct VirtualController {
    steam_controller_hid: SteamControllerHid,
    evdev_controller: Option<EvdevController>,
    dualsense_controller: Option<VirtualDualSenseController>,
}

impl VirtualController {
    pub fn new(steam_controller: &SteamControllerHid) -> eyre::Result<Self> {
        Ok(
            VirtualController {
                steam_controller_hid: steam_controller.clone(),
                evdev_controller: None,
                dualsense_controller: None,
            }
        )
    }

    pub fn use_evdev(&mut self) -> eyre::Result<()> {
        self.evdev_controller = Some(EvdevController::new(&self.steam_controller_hid)?);

        if self.dualsense_controller.is_some() {
            self.dualsense_controller = None;
        }

        Ok(())
    }

    pub fn use_dualsense(&mut self) -> eyre::Result<()> {
        self.dualsense_controller = Some(VirtualDualSenseController::new(&self.steam_controller_hid)?);

        if self.evdev_controller.is_some() {
            self.evdev_controller = None;
        }

        Ok(())
    }

    pub fn send_button_events(&mut self, buttons: Buttons, pressed: bool) -> eyre::Result<()> {
        if let Some(evdev_controller) = self.evdev_controller.as_mut() {
            evdev_controller.send_button_events(buttons, pressed)?;
        } else if let Some(dualsense_controller) = self.dualsense_controller.as_mut() {
            dualsense_controller.update_buttons(buttons.into(), pressed);
        }

        Ok(())
    }

    pub fn send_trigger_events(&mut self, left_trigger: i16, right_trigger: i16) -> eyre::Result<()> {
        if let Some(evdev_controller) = self.evdev_controller.as_mut() {
            evdev_controller.send_trigger_events(left_trigger, right_trigger)?;
        } else if let Some(dualsense_controller) = self.dualsense_controller.as_mut() {
            let left_trigger = ((left_trigger as f32 / i16::MAX as f32) * u8::MAX as f32).ceil() as u8;
            let right_trigger = ((right_trigger as f32 / i16::MAX as f32) * u8::MAX as f32).ceil() as u8;

            dualsense_controller.set_triggers(left_trigger, right_trigger);
        }

        Ok(())
    }

    pub fn send_joystick_events(&mut self, left_stick: JoystickXY, right_stick: JoystickXY) -> eyre::Result<()> {
        if let Some(evdev_controller) = self.evdev_controller.as_mut() {
            let (left_stick_x, left_stick_y) = left_stick.as_steam_controller_stick_data();
            let (right_stick_x, right_stick_y) = right_stick.as_steam_controller_stick_data();

            evdev_controller.send_joystick_events(left_stick_x, left_stick_y, right_stick_x, right_stick_y)?;
        } else if let Some(dualsense_controller) = self.dualsense_controller.as_mut() {
            dualsense_controller.set_sticks(left_stick, right_stick);
        }

        Ok(())
    }

    pub fn send_touchpad_events(
        &mut self,
        left_touchpad_x: i16,
        left_touchpad_y: i16,
        left_touchpad_pressure: i16,
        right_touchpad_x: i16,
        right_touchpad_y: i16,
        right_touchpad_pressure: i16
    ) -> eyre::Result<()> {
        if let Some(evdev_controller) = self.evdev_controller.as_mut() {
            evdev_controller.send_pad_events(
                left_touchpad_x, -left_touchpad_y, left_touchpad_pressure,
                right_touchpad_x, -right_touchpad_y, right_touchpad_pressure
            )?;
        } else if let Some(dualsense_controller) = self.dualsense_controller.as_mut() {
            dualsense_controller.update_touchpad(left_touchpad_x, -left_touchpad_y, left_touchpad_pressure, 0);
            dualsense_controller.update_touchpad(right_touchpad_x, -right_touchpad_y, right_touchpad_pressure, 1);
        }

        Ok(())
    }

    pub fn send_motion_sensor_events(
        &mut self,
        sensor_timestamp_us: u32,
        accel_x: i16,
        accel_y: i16,
        accel_z: i16,
        gyro_x: i16,
        gyro_y: i16,
        gyro_z: i16
    ) -> eyre::Result<()> {
        if let Some(evdev_controller) = self.evdev_controller.as_mut() {
            evdev_controller.send_motion_sensor_events(
                sensor_timestamp_us,
                accel_x, accel_y, accel_z,
                gyro_x, gyro_y, gyro_z
            )?;
        } else if let Some(dualsense_controller) = self.dualsense_controller.as_mut() {
            dualsense_controller.update_motion_sensors(
                accel_x, accel_y, accel_z,
                gyro_x, gyro_y, gyro_z
            );
        }

        Ok(())
    }

    pub fn sync(&mut self) -> eyre::Result<()> {
        // We only need to update the dualsense controller as evdevil handles syncing on its own.
        if let Some(dualsense_controller) = self.dualsense_controller.as_mut() {
            dualsense_controller.update_inputs()?;
        }

        Ok(())
    }
}

/// X and Y axis values are in the range [-1.0, 1.0].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JoystickXY {
    x: f32,
    y: f32,
}

impl JoystickXY {
    pub fn new(x: f32, y: f32) -> Self {
        JoystickXY {
            x: x.clamp(-1.0, 1.0),
            y: y.clamp(-1.0, 1.0),
        }
    }

    pub fn to_linear01(self) -> Self {
        JoystickXY::new(self.x / 2.0 + 0.5, self.y / 2.0 + 0.5)
    }

    pub fn x(&self) -> f32 {
        self.x
    }

    pub fn y(&self) -> f32 {
        self.y
    }

    pub fn from_steam_controller_stick_data((x, y): (i16, i16)) -> Self {
        let x = x as f32 / i16::MAX as f32;
        let y = y as f32 / i16::MAX as f32;

        JoystickXY::new(x, y)
    }

    pub fn as_steam_controller_stick_data(&self) -> (i16, i16) {
        (
            (self.x * i16::MAX as f32).ceil() as i16,
            (self.y * i16::MAX as f32).ceil() as i16,
        )
    }

    pub fn as_dualsense_stick_data(&self) -> (u8, u8) {
        (
            ((self.x * 0.5f32 + 0.5f32) * u8::MAX as f32) as u8,
            ((self.y * 0.5f32 + 0.5f32) * u8::MAX as f32) as u8,
        )
    }
}

bitflags! {
    // Supposedly this is supposed to be 64 bits long, but all buttons are already accounted for with 32 bits.
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Buttons: u32 {
        const BTN_A = 0b00000000000000000000000000000001;
        const BTN_B = 0b00000000000000000000000000000010;
        const BTN_X = 0b000000000000000000001000;
        const BTN_Y = 0b00000000000000000000000000000100;
        const BTN_QUICK_ACCESS = 0b00000000000000000000000000010000;
        const BTN_THUMBR = 0b00000000000000000000000000100000;
        const BTN_SELECT = 0b00000000000000000100000000000000;
        const BTN_R4 = 0b00000000000000000000000010000000;
        const BTN_R5 = 0b00000000000000000000000100000000;
        const BTN_R1 = 0b00000000000000000000001000000000;
        const BTN_DPAD_DOWN = 0b00000000000000000000010000000000;
        const BTN_DPAD_RIGHT = 0b00000000000000000000100000000000;
        const BTN_DPAD_LEFT = 0b00000000000000000001000000000000;
        const BTN_DPAD_UP = 0b00000000000000000010000000000000;
        const BTN_START = 0b00000000000000000000000001000000;
        const BTN_THUMBL = 0b00000000000000001000000000000000;
        const BTN_STEAM = 0b00000000000000010000000000000000;
        const BTN_L4 = 0b00000000000000100000000000000000;
        const BTN_L5 = 0b00000000000001000000000000000000;
        const BTN_L1 = 0b00000000000010000000000000000000;
        const BTN_THUMBR_TOUCH = 0b00000000000100000000000000000000;
        const BTN_RIGHT_PAD_TOUCH = 0b00000000001000000000000000000000;
        const BTN_RIGHT_PAD_CLICK = 0b00000000010000000000000000000000;
        const BTN_R2 = 0b00000000100000000000000000000000;
        const BTN_THUMBL_TOUCH = 0b00000001000000000000000000000000;
        const BTN_LEFT_PAD_TOUCH = 0b00000010000000000000000000000000;
        const BTN_LEFT_PAD_CLICK = 0b00000100000000000000000000000000;
        const BTN_L2 = 0b00001000000000000000000000000000;
        const BTN_GRIPR = 0b00010000000000000000000000000000;
        const BTN_GRIPL = 0b00100000000000000000000000000000;
        const BTN_UNKNOWN_1 = 0b01000000000000000000000000000000;
        const BTN_UNKNOWN_2 = 0b10000000000000000000000000000000;
    }
}

impl From<Buttons> for DualsenseButtons {
    fn from(value: Buttons) -> Self {
        let mut dualsense_buttons = DualsenseButtons::empty();

        for button in value.iter() {
            let dualsense_button = bitflags_match!(button, {
                Buttons::BTN_A => Some(DualsenseButtons::CROSS),
                Buttons::BTN_B => Some(DualsenseButtons::CIRCLE),
                Buttons::BTN_X => Some(DualsenseButtons::SQUARE),
                Buttons::BTN_Y => Some(DualsenseButtons::TRIANGLE),
                Buttons::BTN_R1 => Some(DualsenseButtons::R1),
                Buttons::BTN_L1 => Some(DualsenseButtons::L1),
                Buttons::BTN_R2 => Some(DualsenseButtons::R2),
                Buttons::BTN_L2 => Some(DualsenseButtons::L2),
                Buttons::BTN_RIGHT_PAD_CLICK => Some(DualsenseButtons::TOUCHPAD),
                Buttons::BTN_LEFT_PAD_CLICK => Some(DualsenseButtons::TOUCHPAD),
                Buttons::BTN_THUMBR => Some(DualsenseButtons::R3),
                Buttons::BTN_THUMBL => Some(DualsenseButtons::L3),
                Buttons::BTN_DPAD_UP => Some(DualsenseButtons::DPAD_UP),
                Buttons::BTN_DPAD_DOWN => Some(DualsenseButtons::DPAD_DOWN),
                Buttons::BTN_DPAD_LEFT => Some(DualsenseButtons::DPAD_LEFT),
                Buttons::BTN_DPAD_RIGHT => Some(DualsenseButtons::DPAD_RIGHT),
                Buttons::BTN_START => Some(DualsenseButtons::CREATE),
                Buttons::BTN_SELECT => Some(DualsenseButtons::OPTIONS),
                Buttons::BTN_STEAM => Some(DualsenseButtons::PS),
                Buttons::BTN_QUICK_ACCESS => Some(DualsenseButtons::MUTE),
                _ => None,
            });

            if let Some(dualsense_button) = dualsense_button {
                dualsense_buttons |= dualsense_button;
            }
        }

        dualsense_buttons
    }
}

pub struct EvdevController {
    gamepad: UinputDevice,
    motion_sensors: UinputDevice,
    ff_handle: JoinHandle<()>,
}

impl EvdevController {
    pub fn new(steam_controller: &SteamControllerHid) -> eyre::Result<Self> {
        let device_info = steam_controller.get_device_info();

        let input_id = InputId::new(
            Bus::from_raw(device_info.bus_type() as u16),
            device_info.vendor_id(),
            device_info.product_id(),
            device_info.release_number(),
        );

        let buttons = vec![
            Key::BTN_TR2,
            Key::BTN_TL2,
            Key::BTN_TR,
            Key::BTN_TL,
            Key::BTN_SOUTH,
            Key::BTN_EAST,
            Key::BTN_WEST,
            Key::BTN_NORTH,
            Key::BTN_0,
            Key::BTN_1,
            Key::BTN_2,
            Key::BTN_3,
            Key::BTN_4,
            Key::BTN_5,
            Key::BTN_6,
            Key::BTN_7,
            Key::BTN_DPAD_UP,
            Key::BTN_DPAD_RIGHT,
            Key::BTN_DPAD_LEFT,
            Key::BTN_DPAD_DOWN,
            Key::BTN_SELECT,
            Key::BTN_BASE,
            Key::BTN_MODE,
            Key::BTN_START,
            Key::BTN_THUMBL,
            Key::BTN_THUMBR,
        ];

        let joystick_abs_info = AbsInfo::new(-32767, 32767)
            .with_fuzz(256)
            .with_resolution(6553);
        let touchpad_abs_info = AbsInfo::new(-32767, 32767)
            .with_fuzz(256);
        let trigger_abs_info = AbsInfo::new(0, 32767)
            .with_resolution(5461);
        let abs_axes = vec![
            AbsSetup::new(Abs::X, joystick_abs_info),
            AbsSetup::new(Abs::Y, joystick_abs_info),
            AbsSetup::new(Abs::RX, joystick_abs_info),
            AbsSetup::new(Abs::RY, joystick_abs_info),
            AbsSetup::new(Abs::HAT0X, touchpad_abs_info),
            AbsSetup::new(Abs::HAT0Y, touchpad_abs_info),
            AbsSetup::new(Abs::HAT1X, touchpad_abs_info),
            AbsSetup::new(Abs::HAT1Y, touchpad_abs_info),
            AbsSetup::new(Abs::HAT2X, trigger_abs_info),
            AbsSetup::new(Abs::HAT2Y, trigger_abs_info),
        ];

        let gamepad = UinputDevice::builder()?
            .with_input_id(input_id.clone())?
            .with_phys(device_info.path().to_str().unwrap())?
            .with_keys(buttons)?
            .with_abs_axes(abs_axes)?
            .with_ff_features([ff::Feature::RUMBLE])?
            .with_ff_effects_max(32)?
            .build(&format!("Steam Controller (evdev wrapper for {:?})", device_info.path()))?;

        let accel_abs_info = AbsInfo::new(-32768, 32768)
            .with_fuzz(32)
            .with_resolution(16384);
        let gyro_abs_info = AbsInfo::new(-32768, 32768)
            .with_fuzz(1)
            .with_resolution(16);
        let abs_axes = vec![
            AbsSetup::new(Abs::X, accel_abs_info),
            AbsSetup::new(Abs::Y, accel_abs_info),
            AbsSetup::new(Abs::Z, accel_abs_info),
            AbsSetup::new(Abs::RX, gyro_abs_info),
            AbsSetup::new(Abs::RY, gyro_abs_info),
            AbsSetup::new(Abs::RZ, gyro_abs_info),
        ];

        let motion_sensors = UinputDevice::builder()?
            .with_input_id(input_id)?
            .with_phys(device_info.path().to_str().unwrap())?
            .with_misc([Misc::TIMESTAMP])?
            .with_abs_axes(abs_axes)?
            .build(&format!("Steam Controller (Motion Sensors) (evdev wrapper for {:?})", device_info.path()))?;

        let ff_params = (
            gamepad.try_clone()?,
            steam_controller.clone(),
        );
        let ff_handle = thread::spawn(move || {
            let (gamepad, mut steam_controller) = ff_params;

            loop {
                for res in gamepad.events() {
                    let event = res.unwrap();
                    match event.kind() {
                        EventKind::Uinput(ev) => match ev.code() {
                            UinputCode::FF_UPLOAD => gamepad.ff_upload(&ev, |upload| {
                                // println!("FF_UPLOAD: {:?}", upload);

                                let effect = upload.effect();
                                let kind = effect.kind().unwrap();
                                match kind {
                                    EffectKind::Rumble(rumble) => {
                                        steam_controller.add_rumble_effect(effect.id().raw(), RumbleEffect {
                                            left_strength: rumble.strong_magnitude(),
                                            right_strength: rumble.weak_magnitude(),
                                        });
                                    }
                                    _ => {}
                                }

                                Ok(())
                            }).unwrap(),
                            UinputCode::FF_ERASE => gamepad.ff_erase(&ev, |erase| {
                                // println!("FF_ERASE: {:?}", erase);
                                Ok(())
                            }).unwrap(),
                            _ => {
                                println!("Unhandled event: {:?}", ev);
                            }
                        }
                        EventKind::ForceFeedback(ff_event) => match ff_event.code() {
                            Some(code) => {
                                match code {
                                    ForceFeedbackCode::ControlEffect(effect) => {
                                        println!("ControlEffect: {:?}", effect);
                                        let active = ff_event.raw_value();
                                        if active == 0 {
                                            steam_controller.remove_rumble_effect(effect.raw());
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            _ => {}
                        }
                        _ => {}
                    }
                }
            }
        });

        Ok(
            EvdevController {
                gamepad,
                motion_sensors,
                ff_handle,
            }
        )
    }

    pub fn send_button_events(&mut self, buttons: Buttons, pressed: bool) -> eyre::Result<()> {
        let key_code = bitflags_match!(buttons, {
            Buttons::BTN_A => Some(Key::BTN_SOUTH),
            Buttons::BTN_B => Some(Key::BTN_EAST),
            Buttons::BTN_X => Some(Key::BTN_WEST),
            Buttons::BTN_Y => Some(Key::BTN_NORTH),
            Buttons::BTN_R1 => Some(Key::BTN_TR),
            Buttons::BTN_L1 => Some(Key::BTN_TL),
            Buttons::BTN_R2 => Some(Key::BTN_TR2),
            Buttons::BTN_L2 => Some(Key::BTN_TL2),
            Buttons::BTN_R4 => Some(Key::BTN_0),
            Buttons::BTN_L4 => Some(Key::BTN_1),
            Buttons::BTN_R5 => Some(Key::BTN_2),
            Buttons::BTN_L5 => Some(Key::BTN_3),
            Buttons::BTN_RIGHT_PAD_CLICK => Some(Key::BTN_4),
            Buttons::BTN_LEFT_PAD_CLICK => Some(Key::BTN_5),
            Buttons::BTN_GRIPR => Some(Key::BTN_6),
            Buttons::BTN_GRIPL => Some(Key::BTN_7),
            Buttons::BTN_THUMBL => Some(Key::BTN_THUMBL),
            Buttons::BTN_THUMBR => Some(Key::BTN_THUMBR),
            Buttons::BTN_DPAD_UP => Some(Key::BTN_DPAD_UP),
            Buttons::BTN_DPAD_DOWN => Some(Key::BTN_DPAD_DOWN),
            Buttons::BTN_DPAD_LEFT => Some(Key::BTN_DPAD_LEFT),
            Buttons::BTN_DPAD_RIGHT => Some(Key::BTN_DPAD_RIGHT),
            Buttons::BTN_START => Some(Key::BTN_START),
            Buttons::BTN_SELECT => Some(Key::BTN_SELECT),
            Buttons::BTN_STEAM => Some(Key::BTN_MODE),
            Buttons::BTN_QUICK_ACCESS => Some(Key::BTN_BASE),
            _ => None,
        });

        if let Some(key_code) = key_code {
            let value = if pressed { 1 } else { 0 };
            self.gamepad.write(&[*KeyEvent::new(key_code, KeyState::from_raw(value))])?;
        }

        Ok(())
    }

    pub fn send_trigger_events(&mut self, left_trigger: i16, right_trigger: i16) -> eyre::Result<()> {
        let trigger_events = vec![
            *AbsEvent::new(Abs::HAT2X, left_trigger as i32),
            *AbsEvent::new(Abs::HAT2Y, right_trigger as i32),
        ];

        self.gamepad.write(&trigger_events)?;

        Ok(())
    }

    pub fn send_joystick_events(&mut self, left_joystick_x: i16, left_joystick_y: i16, right_joystick_x: i16, right_joystick_y: i16) -> eyre::Result<()> {
        let axis_events = vec![
            *AbsEvent::new(Abs::X, left_joystick_x as i32),
            *AbsEvent::new(Abs::Y, left_joystick_y as i32),
            *AbsEvent::new(Abs::RX, right_joystick_x as i32),
            *AbsEvent::new(Abs::RY, right_joystick_y as i32),
        ];

        self.gamepad.write(&axis_events)?;

        Ok(())
    }

    pub fn send_pad_events(
        &mut self,
        left_pad_x: i16,
        left_pad_y: i16,
        left_pad_pressure: i16,
        right_pad_x: i16,
        right_pad_y: i16,
        right_pad_pressure: i16,
    ) -> eyre::Result<()> {
        let pad_events = vec![
            *AbsEvent::new(Abs::HAT0X, left_pad_x as i32),
            *AbsEvent::new(Abs::HAT0Y, left_pad_y as i32),
            *AbsEvent::new(Abs::PRESSURE, left_pad_pressure as i32),
            *AbsEvent::new(Abs::HAT1X, right_pad_x as i32),
            *AbsEvent::new(Abs::HAT1Y, right_pad_y as i32),
            *AbsEvent::new(Abs::MT_PRESSURE, right_pad_pressure as i32),
        ];

        self.gamepad.write(&pad_events)?;

        Ok(())
    }

    pub fn send_motion_sensor_events(
        &mut self,
        timestamp_us: u32,
        accel_x: i16,
        accel_y: i16,
        accel_z: i16,
        gyro_x: i16,
        gyro_y: i16,
        gyro_z: i16,
    ) -> eyre::Result<()> {
        let motion_events = vec![
            *MiscEvent::new(Misc::TIMESTAMP, timestamp_us as i32),
            *AbsEvent::new(Abs::X, accel_x as i32),
            *AbsEvent::new(Abs::Y, accel_y as i32),
            *AbsEvent::new(Abs::Z, accel_z as i32),
            *AbsEvent::new(Abs::RX, gyro_x as i32),
            *AbsEvent::new(Abs::RY, gyro_y as i32),
            *AbsEvent::new(Abs::RZ, gyro_z as i32),
        ];

        self.motion_sensors.write(&motion_events)?;

        Ok(())
    }
}
