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

use esp_hal::i2c;
use esp_hal::time::Rate;

use defmt_rtt as _;
use esp_backtrace as _;

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_hal::main]
fn main() -> ! {
    let peripherals = esp_hal::init(esp_hal::Config::default());
    let rtc = esp_hal::rtc_cntl::Rtc::new(peripherals.LPWR);

    let boot_time = rtc.time_since_boot().as_millis();
    let wakeup_cause = esp_hal::rtc_cntl::wakeup_cause();
    let delay = esp_hal::delay::Delay::new();

    defmt::info!("INIT: boot_time={} wakeup={}", boot_time, wakeup_cause);

    esp_alloc::heap_allocator!(size: 64 * 1024);

    // Wait for bus to initialise
    delay.delay_millis(10);

    let i2c_config = i2c::master::Config::default().with_frequency(Rate::from_khz(100));
    let scl = peripherals.GPIO4;
    let sda = peripherals.GPIO5;
    let mut i2c = i2c::master::I2c::new(peripherals.I2C0, i2c_config)
        .expect("Error initailising I2C")
        .with_scl(scl)
        .with_sda(sda);

    defmt::info!("Scan I2C bus: START");
    for addr in 0..=127 {
        if let Ok(_) = i2c.write(addr, &[0]) {
            defmt::info!("Found I2C device at address: 0x{:02x}", addr);
        }
        delay.delay_millis(5);
    }
    defmt::info!("Scan I2C bus: DONE");

    ina219::reset(&mut i2c).unwrap();
    defmt::info!(
        "INA219 Config: {:x}",
        ina219::configuration(&mut i2c).unwrap()
    );

    loop {
        let v = ina219::read(&mut i2c).unwrap();
        defmt::info!("INA219: bus_v={}V shunt_i={}mA", v.bus_v, v.shunt_ma);
        delay.delay_millis(1000);
    }
}

mod ina219 {

    #![allow(unused)]

    use esp_hal::i2c::master::I2c;
    use esp_hal::Blocking;

    const INA219_ADDRESS: u8 = 0x40;
    const INA219_SHUNT_RESISTOR: f32 = 0.1;

    // Registers
    const INA219_CONFIG: u8 = 0x00;
    const INA219_SHUNT_V: u8 = 0x01;
    const INA219_BUS_V: u8 = 0x02;

    // Config bits
    const BRNG_OFFSET: u8 = 13;
    const BRNG_WIDTH: u8 = 1;
    const PGA_OFFSET: u8 = 11;
    const PGA_WIDTH: u8 = 2;
    const BADC_OFFSET: u8 = 7;
    const BADC_WIDTH: u8 = 4;
    const SADC_OFFSET: u8 = 3;
    const SADC_WIDTH: u8 = 4;

    #[derive(Debug, Clone)]
    pub enum Ina219Error {
        I2cError,
    }

    #[derive(Debug, Clone)]
    pub enum Ina219Brng {
        Brng16V = 0b0,
        Brng32V = 0b1,
    }

    #[derive(Debug, Clone)]
    pub enum Ina219Pga {
        Pga40mV = 0b00,
        Pga80mV = 0b01,
        Pga160mV = 0b10,
        Pga320mV = 0b11,
    }

    #[derive(Debug, Clone)]
    pub enum Ina219Adc {
        Adc9 = 0b0000,
        Adc10 = 0b0001,
        Adc11 = 0b0010,
        Adc12 = 0b0011,
        Adc12_1 = 0b1000,
        Adc12_2 = 0b1001,
        Adc12_4 = 0b1010,
        Adc12_8 = 0b1011,
        Adc12_16 = 0b1100,
        Adc12_32 = 0b1101,
        Adc12_64 = 0b1110,
        Adc12_128 = 0b1111,
    }

    #[derive(Debug, Clone)]
    pub struct Ina219Reading {
        pub bus_v: f32,
        pub shunt_ma: f32,
    }

    #[derive(Debug, Clone)]
    pub struct Ina219Config(u16);

    impl Ina219Config {
        // MODE not implemented - default = Shunt & Bus continuous
        pub fn default() -> Self {
            Self(0x399f)
        }
        pub fn with_brng(mut self, brng: Ina219Brng) -> Self {
            self.set_bits(brng as u16, BRNG_OFFSET, BRNG_WIDTH);
            self
        }
        pub fn with_pga(mut self, pga: Ina219Pga) -> Self {
            self.set_bits(pga as u16, PGA_OFFSET, PGA_WIDTH);
            self
        }
        pub fn with_badc(mut self, adc: Ina219Adc) -> Self {
            self.set_bits(adc as u16, BADC_OFFSET, BADC_WIDTH);
            self
        }
        pub fn with_sadc(mut self, adc: Ina219Adc) -> Self {
            self.set_bits(adc as u16, SADC_OFFSET, SADC_WIDTH);
            self
        }
        pub fn as_cmd(&self) -> [u8; 3] {
            let [b1, b2] = self.0.to_be_bytes();
            [INA219_CONFIG, b1, b2]
        }
        #[inline]
        fn set_bits(&mut self, field: u16, offset: u8, width: u8) {
            let mask = ((1u16 << width) - 1) << offset;
            let value = self.0;
            self.0 = (value & !mask) | ((field & (mask >> offset)) << offset);
        }
    }

    pub fn reset(i2c: &mut I2c<'_, Blocking>) -> Result<(), Ina219Error> {
        i2c.write(INA219_ADDRESS, &[0x00, 0x80, 0x00])
            .map_err(|_| Ina219Error::I2cError)?;
        Ok(())
    }

    pub fn read(i2c: &mut I2c<'_, Blocking>) -> Result<Ina219Reading, Ina219Error> {
        let mut buf = [0u8; 2];
        i2c.write_read(INA219_ADDRESS, &[INA219_BUS_V], &mut buf[..])
            .map_err(|_| Ina219Error::I2cError)?;
        defmt::info!(
            "BUS_V: {} {:016b}",
            u16::from_be_bytes(buf),
            u16::from_be_bytes(buf)
        );
        let bus_v = (u16::from_be_bytes(buf) >> 3) as f32 / 250.0; // LSB = 4mV
        i2c.write_read(INA219_ADDRESS, &[INA219_SHUNT_V], &mut buf[..])
            .map_err(|_| Ina219Error::I2cError)?;
        let shunt_mv = i16::from_be_bytes(buf) as f32 / 100.0;
        let shunt_ma = shunt_mv / INA219_SHUNT_RESISTOR;
        Ok(Ina219Reading { bus_v, shunt_ma })
    }

    pub fn set_configuration(
        i2c: &mut I2c<'_, Blocking>,
        config: Ina219Config,
    ) -> Result<u16, Ina219Error> {
        i2c.write(INA219_ADDRESS, &config.as_cmd())
            .map_err(|_| Ina219Error::I2cError)?;
        let mut buf = [0u8; 2];
        i2c.write_read(INA219_ADDRESS, &[INA219_CONFIG], &mut buf[..])
            .map_err(|_| Ina219Error::I2cError)?;
        Ok(u16::from_be_bytes(buf))
    }

    pub fn configuration(i2c: &mut I2c<'_, Blocking>) -> Result<u16, Ina219Error> {
        let mut buf = [0u8; 2];
        i2c.write_read(INA219_ADDRESS, &[INA219_CONFIG], &mut buf[..])
            .map_err(|_| Ina219Error::I2cError)?;
        Ok(u16::from_be_bytes(buf))
    }
}
