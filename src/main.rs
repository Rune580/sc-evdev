use std::collections::HashMap;
use std::sync::{mpsc, Arc, Mutex, RwLock};
use std::sync::mpsc::{Sender, TryRecvError};
use std::thread;
use color_eyre::eyre;
use hidapi::{DeviceInfo, HidDevice};
use crate::virtual_controllers::{Buttons, JoystickXY, VirtualController};

mod virtual_controllers;

fn main() -> eyre::Result<()> {
    let api = hidapi::HidApi::new()?;

    let mut devices: Vec<DeviceInfo> = Vec::new();
    for device in api.device_list() {
        if !devices.iter().any(|existing_device| existing_device.path() == device.path()) {
            devices.push(device.clone());
        }
    }

    let steam_controllers = devices.iter()
        .filter(|device| device.vendor_id() == 0x28de && device.product_id() == 0x1304)
        .cloned()
        .collect::<Vec<DeviceInfo>>();

    let mut controllers: Vec<HidDevice> = Vec::new();
    for device in steam_controllers.iter() {
        let steam_controller = api.open_path(device.path())?;
        controllers.push(steam_controller);
    }

    let (tx, rx) = mpsc::channel::<DaemonState>();
    let mut daemons_running = controllers.len();

    for controller in controllers {
        let tx = tx.clone();
        thread::spawn(move || handle_controller(controller, tx));
    }

    while daemons_running > 0 {
        match rx.recv() {
            Ok(DaemonState::Stopped) => {
                daemons_running -= 1;
            },
            Err(err) => {
                println!("{:?}", err);
                break;
            }
        }
    }

    Ok(())
}

enum DaemonState {
    Stopped,
}

#[repr(u8)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum Command {
    SetDigitalMappings = 0x80,
    ClearDigitalMappings = 0x81,
    GetDigitalMappings = 0x82,
    SetDefaultMappings = 0x85,
    SetSettings = 0x87,
    GetSettings = 0x88,
    Unknown1 = 0xe2,
    Unknown2 = 0xdc,
    Unknown3 = 0xc1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RumbleEffect {
    pub left_strength: u16,
    pub right_strength: u16,
}


#[derive(Debug, Clone)]
struct SteamControllerHid {
    device: Arc<Mutex<HidDevice>>,
    device_info: DeviceInfo,
    rumble_effects: Arc<RwLock<HashMap<i16, RumbleEffect>>>,
}

impl SteamControllerHid {
    const FEATURE_REPORT_CMD: u8 = 0x01;
    const SETTING_LEFT_TRACKPAD_MODE: u8 = 0x07;
    const SETTING_RIGHT_TRACKPAD_MODE: u8 = 0x08;
    const SETTING_LEFT_TRACKPAD_CLICK_PRESSURE: u8 = 52;
    const SETTING_RIGHT_TRACKPAD_CLICK_PRESSURE: u8 = 53;
    const SETTING_STEAM_WATCHDOG_ENABLE: u8 = 71;

    pub fn new(device: HidDevice) -> Self {
        let device_info = device.get_device_info().unwrap();

        SteamControllerHid {
            device: Arc::new(Mutex::new(device)),
            device_info,
            rumble_effects: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn get_device_info(&self) -> &DeviceInfo {
        &self.device_info
    }

    pub fn read(&self, buf: &mut [u8]) -> eyre::Result<usize> {
        let device = self.device.lock()
            .map_err(|e| eyre::eyre!("Failed to lock device: {e}"))?;
        let read = device.read(buf)?;
        Ok(read)
    }

    pub fn send_command(&self, command: Command) -> eyre::Result<()> {
        self.send_command_with_payload(command, &[])
    }

    pub fn send_command_with_payload(&self, command: Command, payload: &[u8]) -> eyre::Result<()> {
        let size = payload.len();

        let mut buf = vec![Self::FEATURE_REPORT_CMD, command as u8, size as u8];
        buf.extend_from_slice(payload);

        while buf.len() < 64 {
            buf.push(0x00);
        }

        let device = self.device.lock()
            .map_err(|e| eyre::eyre!("Failed to lock device: {e}"))?;

        let mut retries = 50;
        while let Err(e) = device.send_feature_report(&buf) {
            retries -= 1;
            if retries <=0 {
                return Err(eyre::eyre!("Failed to send feature report after 50 retries: {}", e));
            }
        }

        Ok(())
    }

    pub fn add_rumble_effect(&mut self, id: i16, rumble_effect: RumbleEffect) {
        let mut rumble_effects = self.rumble_effects.write().unwrap();
        rumble_effects.insert(id, rumble_effect);
    }

    pub fn remove_rumble_effect(&mut self, id: i16) {
        let mut rumble_effects = self.rumble_effects.write().unwrap();
        rumble_effects.remove(&id);
    }

    pub fn process_rumble_effects(&self) {
        let rumble_effects = self.rumble_effects.read().unwrap();
        for (id, rumble_effect) in rumble_effects.iter() {
            self.send_rumble(rumble_effect.left_strength, rumble_effect.right_strength).unwrap();
        }
    }

    pub fn send_rumble(&self, left_rumble: u16, right_rumble: u16) -> eyre::Result<()> {
        // println!("Sending rumble: {:04x} {:04x}", left_rumble, right_rumble);

        let device = self.device.lock()
            .map_err(|e| eyre::eyre!("Failed to lock device: {e}"))?;
        device.write(&[
            0x80,
            0x00, 0x00, // Intensity
            (left_rumble & 0xFF) as u8, (left_rumble >> 8) as u8,
            (right_rumble & 0xFF) as u8, (right_rumble >> 8) as u8,
            2, // Left Gain
            0, // Right Gain
            0x00,
        ])?;

        Ok(())
    }
}

// copied from https://github.com/torvalds/linux/blob/50897c955902c93ae71c38698abb910525ebdc89/drivers/hid/hid-steam.c#L1357
fn i16_from_le_bytes_steam(bytes: &[u8]) -> eyre::Result<i16> {
    let mut short = i16::from_le_bytes(bytes.try_into()?);

    if short == i16::MIN {
        short = i16::MIN + 1;
    }

    Ok(short)
}

fn handle_controller(controller: HidDevice, tx: Sender<DaemonState>) {
    let mut virtual_controller = None;
    let controller = SteamControllerHid::new(controller);

    let (write_thread_tx, rx) = mpsc::channel::<bool>();

    let write_thread_params = (
        controller.clone(),
    );
    let write_thread = thread::spawn(move || {
        let (controller, ) = write_thread_params;

        controller.send_command(Command::ClearDigitalMappings).unwrap();

        // Enable Motion Sensors.
        controller.send_command_with_payload(Command::SetSettings, &[
            0x18, 0x00, 0x00,
            0x2e, 0x00, 0x00,
            0x34, 0xff, 0xff,
            0x35, 0xff, 0xff,
            0x2e, 0x00, 0x00,
        ]).unwrap();

        loop {
            // Disable lizard mode.
            controller.send_command_with_payload(Command::SetSettings, &[
                0x09, 0x00, 0x00,
            ]).unwrap();

            controller.process_rumble_effects();

            match rx.try_recv() {
                Ok(should_exit) => {
                    if should_exit {
                        return;
                    }
                }
                Err(err) => {
                    match err {
                        TryRecvError::Disconnected => {
                            return;
                        }
                        _ => {}
                    }
                }
            }
            thread::sleep(std::time::Duration::from_millis(50));
        }
    });

    let mut buf = [0u8; 1024];

    let mut last_button_state = Buttons::empty();

    loop {
        let read = controller.read(&mut buf);
        if let Ok(read) = read {
            // println!("{:?}", &buf[..read]);

            if virtual_controller.is_none() {
                let mut v = VirtualController::new(&controller).unwrap();

                v.use_dualsense().unwrap();
                // v.use_evdev().unwrap();

                virtual_controller = Some(v);
            }
            let virtual_controller = virtual_controller.as_mut().unwrap();

            let report_id = buf[0];

            if report_id == 69 {
                let buttons_u32 = u32::from_le_bytes(buf[2..6].try_into().unwrap());

                let buttons = Buttons::from_bits_truncate(buttons_u32);

                let released = last_button_state.difference(buttons);
                let pressed = buttons.difference(last_button_state);
                last_button_state = buttons;

                if !pressed.is_empty() || !released.is_empty() {
                    released.iter().for_each(|button| {
                        virtual_controller.send_button_events(button, false).unwrap();
                    });
                    pressed.iter().for_each(|button| {
                        virtual_controller.send_button_events(button, true).unwrap();
                    });
                }

                let left_trigger = i16_from_le_bytes_steam(&buf[6..8]).unwrap();
                let right_trigger = i16_from_le_bytes_steam(&buf[8..10]).unwrap();
                virtual_controller.send_trigger_events(left_trigger, right_trigger).unwrap();

                let left_joystick_x = i16_from_le_bytes_steam(&buf[10..12]).unwrap();
                let left_joystick_y = i16_from_le_bytes_steam(&buf[12..14]).unwrap();
                let right_joystick_x = i16_from_le_bytes_steam(&buf[14..16]).unwrap();
                let right_joystick_y = i16_from_le_bytes_steam(&buf[16..18]).unwrap();

                let left_joystick = JoystickXY::from_steam_controller_stick_data((left_joystick_x, -left_joystick_y));
                let right_joystick = JoystickXY::from_steam_controller_stick_data((right_joystick_x, -right_joystick_y));
                virtual_controller.send_joystick_events(left_joystick, right_joystick).unwrap();

                let left_pad_x = i16_from_le_bytes_steam(&buf[18..20]).unwrap();
                let left_pad_y = i16_from_le_bytes_steam(&buf[20..22]).unwrap();
                let left_pad_pressure = i16_from_le_bytes_steam(&buf[22..24]).unwrap();

                let right_pad_x = i16_from_le_bytes_steam(&buf[24..26]).unwrap();
                let right_pad_y = i16_from_le_bytes_steam(&buf[26..28]).unwrap();
                let right_pad_pressure = i16_from_le_bytes_steam(&buf[28..30]).unwrap();

                virtual_controller.send_touchpad_events(
                    left_pad_x, left_pad_y, left_pad_pressure,
                    right_pad_x, right_pad_y, right_pad_pressure,
                ).unwrap();

                let sensor_timestamp_us = u32::from_le_bytes(buf[30..34].try_into().unwrap());

                // Not sure if this is actually the accelerometer.
                let accel_x = i16_from_le_bytes_steam(&buf[34..36]).unwrap();
                let accel_y = i16_from_le_bytes_steam(&buf[36..38]).unwrap();
                let accel_z = i16_from_le_bytes_steam(&buf[38..40]).unwrap();
                // println!("Accel: {:?} {:?} {:?}", accel_x, accel_y, accel_z);

                let gyro_x = i16_from_le_bytes_steam(&buf[40..42]).unwrap();
                let gyro_y = i16_from_le_bytes_steam(&buf[42..44]).unwrap();
                let gyro_z = i16_from_le_bytes_steam(&buf[44..46]).unwrap();

                // println!("Sensor Data: ( ts: {sensor_timestamp_us}, accel: {accel_x} {accel_y} {accel_z}, gyro: {gyro_x} {gyro_y} {gyro_z} )");

                virtual_controller.send_motion_sensor_events(
                    sensor_timestamp_us,
                    accel_x, accel_y, accel_z,
                    gyro_x, gyro_y, gyro_z,
                ).unwrap();

                // println!("{:02x?}", &buf[30..read]);
            } else {
                // println!("{:?}", &buf[..read]);
            }

            virtual_controller.sync().unwrap();
        } else {
            println!("{:?}", read);

            tx.send(DaemonState::Stopped).unwrap();
            break;
        }
    }

    write_thread_tx.send(true).unwrap();
    write_thread.join().unwrap();
}