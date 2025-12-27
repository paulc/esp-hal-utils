#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]
#![feature(proc_macro_hygiene)]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

extern crate alloc;

use esp_hal::analog::adc::{Adc, AdcCalBasic, AdcConfig, Attenuation};
use esp_hal::gpio::{DriveStrength, Level, Output, OutputConfig};
use esp_hal::i2c;
use esp_hal::rtc_cntl::sleep::TimerWakeupSource;
use esp_hal::system::SleepSource;
use esp_hal::time::Rate;
use esp_hal::timer::timg::TimerGroup;

#[cfg(target_arch = "riscv32")]
use esp_hal::interrupt::software::SoftwareInterruptControl;

use esp_hal_utils::format_mac::format_mac;

use defmt_rtt as _;
use esp_backtrace as _;

use core::fmt::Write;

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_hal::main]
fn main() -> ! {
    let peripherals = esp_hal::init(esp_hal::Config::default());
    let mut rtc = esp_hal::rtc_cntl::Rtc::new(peripherals.LPWR);
    let timer = TimerWakeupSource::new(core::time::Duration::from_secs(10));

    let boot_time = rtc.time_since_boot().as_millis();
    let wakeup_cause = esp_hal::rtc_cntl::wakeup_cause();
    let delay = esp_hal::delay::Delay::new();

    defmt::info!("INIT: boot_time={} wakeup={}", boot_time, wakeup_cause);

    esp_alloc::heap_allocator!(size: 64 * 1024);

    #[cfg(target_arch = "riscv32")]
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(
        timg0.timer0,
        #[cfg(target_arch = "riscv32")]
        sw_int.software_interrupt0,
    );

    defmt::info!("ESP_RTOS initialized!");

    if let SleepSource::Undefined = wakeup_cause {
        // Clear rtc
        hub::clear_rtc();
        bmp280_blocking::clear_rtc();
        for i in 0..5 {
            defmt::info!("WAIT [{}]", i);
            delay.delay_millis(1000);
        }
    }

    #[cfg(feature = "esp32c6")]
    let mut led = Output::new(peripherals.GPIO15, Level::Low, OutputConfig::default());
    #[cfg(not(feature = "esp32c6"))]
    let mut led = Output::new(peripherals.GPIO1, Level::Low, OutputConfig::default());

    // Onboard LED
    led.set_high();

    // Power to I2C sensor
    let mut i2c_pwr = Output::new(
        peripherals.GPIO7,
        Level::High,
        OutputConfig::default().with_drive_strength(DriveStrength::_40mA),
    );
    defmt::info!("Enabling sensor module power");

    // Initialise ESP_NOW
    defmt::info!("Initialise ESP_NOW");
    let esp_radio_ctrl = esp_radio::init().unwrap();
    let wifi = peripherals.WIFI;
    let (mut controller, interfaces) =
        esp_radio::wifi::new(&esp_radio_ctrl, wifi, Default::default()).unwrap();
    controller.set_mode(esp_radio::wifi::WifiMode::Sta).unwrap();
    controller.start().unwrap();

    // Initialise ADC
    let adc_pin = peripherals.GPIO0;
    let mut adc_config = AdcConfig::new();
    let mut pin = adc_config.enable_pin_with_cal::<_, AdcCalBasic<_>>(adc_pin, Attenuation::_11dB);
    let mut adc = Adc::new(peripherals.ADC1, adc_config);

    let adc_avg = (0..5)
        .map(|_| {
            let adc_raw = adc.read_oneshot(&mut pin).unwrap() as u32;
            // let adc_v = adc_raw as f32 * 1.07 / 1000.0; // 1.07mV per code point
            // XXX With 2 x 1M voltage divider ADC pin resistance is too high XXX
            let adc_v = 2.0 * adc_raw as f32 * 1.5 / 1000.0; // ~1.5mV per code point with 1M divider
                                                             // 2x Voltage Divider
            defmt::info!("Battery Voltage: {}V [{}]", adc_v, adc_raw);
            delay.delay_millis(5);
            adc_v
        })
        .sum::<f32>()
        / 5.0;

    // Initialise I2C in separate scope to ensure it is dropped
    let (bmp280_t, bmp280_p, aht20_t, aht20_rh) = {
        let i2c_config = i2c::master::Config::default().with_frequency(Rate::from_khz(100));
        let scl = peripherals.GPIO4;
        let sda = peripherals.GPIO5;
        let mut i2c = i2c::master::I2c::new(peripherals.I2C0, i2c_config)
            .expect("Error initailising I2C")
            .with_scl(scl)
            .with_sda(sda);

        bmp280_blocking::initialise(&mut i2c).unwrap();
        aht20_blocking::initialise(&mut i2c).unwrap();

        let (bmp280_t, bmp280_p) = bmp280_blocking::measure(&mut i2c).unwrap();
        defmt::info!("BM280 -> Temp = {}C Pressure = {}hPa", bmp280_t, bmp280_p);

        let (aht20_t, aht20_rh) = aht20_blocking::read(&mut i2c).unwrap();
        defmt::info!(
            "AHT20 -> Temp = {}C Relative Humidity = {}",
            aht20_t,
            aht20_rh
        );

        (bmp280_t, bmp280_p, aht20_t, aht20_rh)
    };

    let mut esp_now = interfaces.esp_now;
    esp_now.set_channel(11).unwrap();

    defmt::info!("ESP-NOW VERSION: {}", esp_now.version().unwrap());
    defmt::info!(
        "        MAC ADDRESS: {}",
        format_mac(&esp_radio::wifi::sta_mac())
    );

    let hub_address = hub::find_hub(&mut esp_now, delay.clone());

    let mut buf = heapless::String::<256>::new();

    write!(
        &mut buf,
        "C6-SENSOR: [{}] bmp280 <t: {} p: {}> aht20 <t: {} rh: {}> battery: <v: {}>",
        boot_time / 1000,
        bmp280_t,
        bmp280_p,
        aht20_t,
        aht20_rh,
        adc_avg
    )
    .unwrap();

    let status = esp_now.send(&hub_address, buf.as_bytes()).unwrap().wait();
    defmt::info!("ESP-NOW: TX (blocking) -> {:?}", status);

    // Turn off I2C power
    i2c_pwr.set_low();

    // Force I2C pins into Input mode to retuce leakage before sleep
    unsafe {
        use esp_hal::gpio::{AnyPin, Input, InputConfig, Pull};

        let gpio4 = AnyPin::steal(4);
        let gpio5 = AnyPin::steal(5);
        let _ = Input::new(gpio4, InputConfig::default().with_pull(Pull::None));
        let _ = Input::new(gpio5, InputConfig::default().with_pull(Pull::None));
    }

    defmt::info!("SLEEPING:");
    if let SleepSource::Undefined = wakeup_cause {
        for i in 0..2 {
            defmt::info!("WAIT [{}]", i);
            delay.delay_millis(1000);
        }
    }

    rtc.sleep_deep(&[&timer]);
}

mod hub {

    use esp_hal_utils::crc::crc16;
    use esp_hal_utils::format_mac::format_mac;
    use esp_radio::esp_now::{EspNow, PeerInfo, BROADCAST_ADDRESS};

    // MAGIC = 2 bytes, ADDRESS = 6 bytes, CRC = 2 bytes
    #[derive(Clone, Copy)]
    #[repr(transparent)]
    pub struct HubAddressRtc([u8; 10]);

    unsafe impl esp_hal::Persistable for HubAddressRtc {}

    impl HubAddressRtc {
        fn pack(address: [u8; 6]) -> Self {
            let mut buf = [0_u8; 10];
            buf[0..2].copy_from_slice(&(0x1111_u16).to_le_bytes());
            buf[2..8].copy_from_slice(&address);
            let crc = crc16(&buf[0..8]).to_le_bytes();
            buf[8..10].copy_from_slice(&crc);
            Self(buf)
        }
        fn check(&self) -> bool {
            let crc = crc16(&self.0[0..8]).to_le_bytes();
            crc == self.0[8..10]
        }
        fn unpack(&self) -> Option<[u8; 6]> {
            if self.check() {
                let mut out = [0_u8; 6];
                out.copy_from_slice(&self.0[2..8]);
                Some(out)
            } else {
                None
            }
        }
    }

    #[esp_hal::ram(unstable(rtc_fast, persistent))]
    static mut HUB_ADDRESS: HubAddressRtc = HubAddressRtc([0_u8; 10]);

    pub fn clear_rtc() {
        unsafe { HUB_ADDRESS = HubAddressRtc([0_u8; 10]) }
    }

    pub fn find_hub(esp_now: &mut EspNow<'_>, delay: esp_hal::delay::Delay) -> [u8; 6] {
        let hub_rtc = unsafe { HUB_ADDRESS };
        match hub_rtc.unpack() {
            Some(hub) => {
                // Stored hub address
                defmt::info!("USING STORED HUB ADDRESS");
                defmt::info!("ESP-NOW ADD PEER: {}", format_mac(&hub));
                esp_now
                    .add_peer(PeerInfo {
                        interface: esp_radio::esp_now::EspNowWifiInterface::Sta,
                        peer_address: hub,
                        lmk: None,
                        channel: None,
                        encrypt: false,
                    })
                    .unwrap();
                hub
            }
            None => {
                defmt::info!("SEARCHING FOR HUB BROADCAST");
                loop {
                    if let Some(r) = esp_now.receive() {
                        if r.info.dst_address == BROADCAST_ADDRESS && r.data() == b"<<HUB>>" {
                            defmt::info!(">> RX HUB BROADCAST");
                            if !esp_now.peer_exists(&r.info.src_address) {
                                defmt::info!(
                                    "ESP-NOW ADD PEER: {}",
                                    format_mac(&r.info.src_address)
                                );
                                esp_now
                                    .add_peer(PeerInfo {
                                        interface: esp_radio::esp_now::EspNowWifiInterface::Sta,
                                        peer_address: r.info.src_address,
                                        lmk: None,
                                        channel: None,
                                        encrypt: false,
                                    })
                                    .unwrap();
                            }
                            unsafe { HUB_ADDRESS = HubAddressRtc::pack(r.info.src_address) };
                            break r.info.src_address;
                        }
                    }
                    delay.delay_millis(50);
                }
            }
        }
    }
}

mod aht20_blocking {

    use esp_hal::i2c::master::I2c;
    use esp_hal::Blocking;

    use esp_hal_utils::crc::crc8;

    const AHT20_ADDRESS: u8 = 0x38;

    #[derive(Debug, Clone)]
    pub enum Aht20Error {
        I2cError,
        CrcError,
    }

    pub fn initialise(i2c: &mut I2c<'_, Blocking>) -> Result<(), Aht20Error> {
        let delay = esp_hal::delay::Delay::new();
        // RESET
        i2c.write(AHT20_ADDRESS, &[0xBA])
            .map_err(|_| Aht20Error::I2cError)?;
        delay.delay_millis(40);
        // INIT
        i2c.write(AHT20_ADDRESS, &[0xBE, 0x08, 0x00])
            .map_err(|_| Aht20Error::I2cError)?;
        delay.delay_millis(10);
        Ok(())
    }
    pub fn read(i2c: &mut I2c<'_, Blocking>) -> Result<(f32, f32), Aht20Error> {
        let delay = esp_hal::delay::Delay::new();
        i2c.write(AHT20_ADDRESS, &[0xAC, 0x33, 0x00])
            .map_err(|_| Aht20Error::I2cError)?;

        // Poll device for ready instead of waiting 80ms
        let mut buf = [0u8; 7];
        loop {
            i2c.read(AHT20_ADDRESS, &mut buf[..1])
                .map_err(|_| Aht20Error::I2cError)?;
            if buf[0] & 0x80 == 0 {
                // Bit 7 = 0 means not busy
                break;
            }
            delay.delay_millis(10);
        }

        // Read result
        i2c.read(AHT20_ADDRESS, &mut buf)
            .map_err(|_| Aht20Error::I2cError)?;

        // Check CRC
        if crc8(&buf[..6]) != buf[6] {
            return Err(Aht20Error::CrcError);
        }

        let rh = (((buf[1] as u32) << 12) | ((buf[2] as u32) << 4) | ((buf[3] as u32) >> 4)) as f32;
        let temp =
            ((((buf[3] as u32) & 0x0F) << 16) | ((buf[4] as u32) << 8) | (buf[5] as u32)) as f32;

        Ok((temp * 200.0 / 1_048_576.0 - 50.0, rh * 100.0 / 1_048_576.0))
    }
}

mod bmp280_blocking {

    use esp_hal::i2c::master::Error as I2cError;
    use esp_hal::i2c::master::I2c;
    use esp_hal::Blocking;

    use esp_hal_utils::crc::crc16;

    // MAGIC = 2 bytes, 12 x f32 = 48 bytes, CRC = 2 byes
    #[derive(Clone, Copy)]
    #[repr(transparent)]
    pub struct Bmp280CalRtc([u8; 52]);

    unsafe impl esp_hal::Persistable for Bmp280CalRtc {}

    #[esp_hal::ram(unstable(rtc_fast, persistent))]
    static mut BMP280_CAL: Bmp280CalRtc = Bmp280CalRtc([0_u8; 52]);

    impl Bmp280CalRtc {
        fn pack(t_calib: [f32; 3], p_calib: [f32; 9]) -> Self {
            let mut buf = [0_u8; 52];
            buf[0..2].copy_from_slice(&(0x2222_u16).to_le_bytes());
            for (i, v) in t_calib.iter().enumerate() {
                let start = 2 + (i * 4);
                let end = start + 4;
                buf[start..end].copy_from_slice(&v.to_le_bytes())
            }
            for (i, v) in p_calib.iter().enumerate() {
                let start = 14 + (i * 4);
                let end = start + 4;
                buf[start..end].copy_from_slice(&v.to_le_bytes())
            }
            let crc = crc16(&buf[0..50]).to_le_bytes();
            buf[50..52].copy_from_slice(&crc);
            Self(buf)
        }
        fn check(&self) -> bool {
            let crc = crc16(&self.0[0..50]).to_le_bytes();
            crc == self.0[50..52]
        }
        fn unpack(&self) -> Option<([f32; 3], [f32; 9])> {
            if self.check() {
                let mut t_calib = [0_f32; 3];
                let mut p_calib = [0_f32; 9];
                for (i, v) in t_calib.iter_mut().enumerate() {
                    let start = 2 + (i * 4);
                    let end = start + 4;
                    let mut buf = [0_u8; 4];
                    buf.copy_from_slice(&self.0[start..end]);
                    *v = f32::from_le_bytes(buf);
                }
                for (i, v) in p_calib.iter_mut().enumerate() {
                    let start = 14 + (i * 4);
                    let end = start + 4;
                    let mut buf = [0_u8; 4];
                    buf.copy_from_slice(&self.0[start..end]);
                    *v = f32::from_le_bytes(buf);
                }
                Some((t_calib, p_calib))
            } else {
                None
            }
        }
    }

    pub fn clear_rtc() {
        unsafe { BMP280_CAL = Bmp280CalRtc([0_u8; 52]) }
    }

    pub fn get_cal(i2c: &mut I2c<'_, Blocking>) -> Result<([f32; 3], [f32; 9]), I2cError> {
        let cal = unsafe { BMP280_CAL };
        match cal.unpack() {
            Some((t_calib, p_calib)) => {
                defmt::info!("Found BMP280 RTC Cal data");
                Ok((t_calib, p_calib))
            }
            None => {
                let mut buf = [0u8; 24];
                defmt::info!("Getting BMP280 Cal data");
                read_register(i2c, BMP280_ADDRESS, 0x88, &mut buf)?;

                let t_calib = [
                    u16::from_le_bytes([buf[0], buf[1]]) as f32,
                    i16::from_le_bytes([buf[2], buf[3]]) as f32,
                    i16::from_le_bytes([buf[4], buf[5]]) as f32,
                ];
                let p_calib = [
                    u16::from_le_bytes([buf[6], buf[7]]) as f32,
                    i16::from_le_bytes([buf[8], buf[9]]) as f32,
                    i16::from_le_bytes([buf[10], buf[11]]) as f32,
                    i16::from_le_bytes([buf[12], buf[13]]) as f32,
                    i16::from_le_bytes([buf[14], buf[15]]) as f32,
                    i16::from_le_bytes([buf[16], buf[17]]) as f32,
                    i16::from_le_bytes([buf[18], buf[19]]) as f32,
                    i16::from_le_bytes([buf[20], buf[21]]) as f32,
                    i16::from_le_bytes([buf[22], buf[23]]) as f32,
                ];
                unsafe { BMP280_CAL = Bmp280CalRtc::pack(t_calib, p_calib) }
                Ok((t_calib, p_calib))
            }
        }
    }

    pub fn read_register(
        i2c: &mut I2c<'_, Blocking>,
        address: u8,
        register: u8,
        buf: &mut [u8],
    ) -> Result<(), I2cError> {
        i2c.write(address, &[register])?;
        i2c.read(address, buf)
    }

    const BMP280_ADDRESS: u8 = 0x77;

    pub fn initialise(i2c: &mut I2c<'_, Blocking>) -> Result<(), I2cError> {
        let delay = esp_hal::delay::Delay::new();
        // RESET
        i2c.write(BMP280_ADDRESS, &[0xE0, 0xB6])?;
        delay.delay_millis(10);

        // CONFIG - Standby::T500, Filter::X16
        let buf: [u8; 2] = [0xF5, (0b100_u8 << 5) | (0b100_u8 << 2)];
        i2c.write(BMP280_ADDRESS, &buf)?;

        // CTRL_MEAS - Oversample::X1, Oversample::X16, Mode::Normal
        let buf: [u8; 2] = [0xF4, (0b001_u8 << 5) | (0b101_u8 << 2) | 0b11];
        i2c.write(BMP280_ADDRESS, &buf)
    }

    pub fn measure(i2c: &mut I2c<'_, Blocking>) -> Result<(f32, f32), I2cError> {
        let (t_calib, p_calib) = get_cal(i2c)?;
        // Wait for status != measuring
        loop {
            let mut buf = [0_u8];
            read_register(i2c, BMP280_ADDRESS, 0xF3, &mut buf)?;
            if (buf[0] & 0b0000_1000) == 0 {
                break;
            }
        }
        // Read Temperature
        let mut buf = [0_u8; 3];
        read_register(i2c, BMP280_ADDRESS, 0xFA, &mut buf)?;
        let t_raw =
            (((buf[0] as u32) << 12) + ((buf[1] as u32) << 4) + ((buf[2] as u32) & 0xf)) as f32;
        // Read Pressure
        read_register(i2c, BMP280_ADDRESS, 0xF7, &mut buf)?;
        let p_raw =
            (((buf[0] as u32) << 12) + ((buf[1] as u32) << 4) + ((buf[2] as u32) & 0xf)) as f32;
        Ok(compensate(t_raw, p_raw, t_calib, p_calib))
    }

    fn compensate(
        t_raw: f32,
        p_raw: f32,
        [t1, t2, t3]: [f32; 3],
        [p1, p2, p3, p4, p5, p6, p7, p8, p9]: [f32; 9],
    ) -> (f32, f32) {
        let mut var1: f32;
        let mut var2: f32;
        // Calculate t_fine
        let t_fine = {
            var1 = (t_raw / 16384.0 - t1 / 1024.0) * t2;
            var2 = ((t_raw / 131072.0 - t1 / 8192.0) * (t_raw / 131072.0 - t1 / 8192.0)) * t3;
            var1 + var2
        };
        // Calculate p
        let p = {
            var1 = (t_fine / 2.0) - 64000.0;
            var2 = (var1 * var1 * p6) / 32768.0;
            var2 = var2 + var1 * p5 * 2.0;
            var2 = (var2 / 4.0) + (p4 * 65536.0);
            var1 = (p3 * var1 * var1 / 524288.0 + p2 * var1) / 524288.0;
            var1 = (1.0 + var1 / 32768.0) * p1;
            if var1 == 0.0 {
                0.0
            } else {
                let mut p = 1048576.0 - p_raw;
                p = (p - (var2 / 4096.0)) * 6250.0 / var1;
                var1 = p9 * p * p / 2147483648.0;
                var2 = p * p8 / 32768.0;
                p = p + (var1 + var2 + p7) / 16.0;
                // Convert from Pa -> HPa
                p / 100.0
            }
        };
        (t_fine / 5120.0, p)
    }
}
