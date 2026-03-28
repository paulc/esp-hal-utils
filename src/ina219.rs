#![allow(unused)]

use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_sync::blocking_mutex::raw::RawMutex;
use embedded_hal_async::i2c::I2c;

pub const INA219_ADDRESS: u8 = 0x40;
pub const INA219_SHUNT_RESISTOR: f32 = 0.1;

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
    Overflow,
    NotReady,
}

#[derive(Debug, Clone)]
pub enum Ina219Brng {
    Brng16V = 0b0,
    Brng32V = 0b1,
}

impl Ina219Brng {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Brng16V => "16V",
            Self::Brng32V => "32V",
        }
    }
}

#[derive(Debug, Clone)]
pub enum Ina219Pga {
    Pga40mV = 0b00,
    Pga80mV = 0b01,
    Pga160mV = 0b10,
    Pga320mV = 0b11,
}

impl Ina219Pga {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pga40mV => "40mV",
            Self::Pga80mV => "80mV",
            Self::Pga160mV => "160mV",
            Self::Pga320mV => "320mV",
        }
    }
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

impl Ina219Adc {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Adc9 => "9-bit",
            Self::Adc10 => "10-bit",
            Self::Adc11 => "11-bit",
            Self::Adc12 => "12-bit",
            Self::Adc12_1 => "12-bit (1 sample)",
            Self::Adc12_2 => "12-bit (2 samples)",
            Self::Adc12_4 => "12-bit (4 samples)",
            Self::Adc12_8 => "12-bit (8 samples)",
            Self::Adc12_16 => "12-bit (16 samples)",
            Self::Adc12_32 => "12-bit (32 samples)",
            Self::Adc12_64 => "12-bit (64 samples)",
            Self::Adc12_128 => "12-bit (128 samples)",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Ina219Reading {
    pub bus_v: f32,
    pub shunt_ma: f32,
}

#[derive(Debug, Clone)]
pub struct Ina219Config(pub u16);

impl Ina219Config {
    // Default = 32V FSR, PGA/8, 12bit ADC, Shunt & Bus continuous
    pub fn default() -> Self {
        Self(0x399f)
    }
    pub fn with_brng(&self, brng: Ina219Brng) -> Self {
        Self(set_bits(self.0, brng as u16, BRNG_OFFSET, BRNG_WIDTH))
    }
    pub fn get_brng(&self) -> Ina219Brng {
        match get_bits(self.0, BRNG_OFFSET, BRNG_WIDTH) {
            0b0 => Ina219Brng::Brng16V,
            0b1 => Ina219Brng::Brng32V,
            _ => panic!("Invalid BRNG Value"), // Not reachable
        }
    }
    pub fn with_pga(&self, pga: Ina219Pga) -> Self {
        Self(set_bits(self.0, pga as u16, PGA_OFFSET, PGA_WIDTH))
    }
    pub fn get_pga(&self) -> Ina219Pga {
        match get_bits(self.0, PGA_OFFSET, PGA_WIDTH) {
            0b00 => Ina219Pga::Pga40mV,
            0b01 => Ina219Pga::Pga80mV,
            0b10 => Ina219Pga::Pga160mV,
            0b11 => Ina219Pga::Pga320mV,
            _ => panic!("Invalid PGA Value"), // Not reachable
        }
    }
    pub fn with_badc(&self, adc: Ina219Adc) -> Self {
        Self(set_bits(self.0, adc as u16, BADC_OFFSET, BADC_WIDTH))
    }
    pub fn get_badc(&self) -> Ina219Adc {
        match get_bits(self.0, BADC_OFFSET, BADC_WIDTH) {
            0b0000 => Ina219Adc::Adc9,
            0b0001 => Ina219Adc::Adc10,
            0b0010 => Ina219Adc::Adc11,
            0b0011 => Ina219Adc::Adc12,
            0b1000 => Ina219Adc::Adc12_1,
            0b1001 => Ina219Adc::Adc12_2,
            0b1010 => Ina219Adc::Adc12_4,
            0b1011 => Ina219Adc::Adc12_8,
            0b1100 => Ina219Adc::Adc12_16,
            0b1101 => Ina219Adc::Adc12_32,
            0b1110 => Ina219Adc::Adc12_64,
            0b1111 => Ina219Adc::Adc12_128,
            _ => panic!("Invalid ADC Value"), // Not reachable
        }
    }
    pub fn with_sadc(&self, adc: Ina219Adc) -> Self {
        Self(set_bits(self.0, adc as u16, SADC_OFFSET, SADC_WIDTH))
    }
    pub fn get_sadc(&self) -> Ina219Adc {
        match get_bits(self.0, SADC_OFFSET, SADC_WIDTH) {
            0b0000 => Ina219Adc::Adc9,
            0b0001 => Ina219Adc::Adc10,
            0b0010 => Ina219Adc::Adc11,
            0b0011 => Ina219Adc::Adc12,
            0b1000 => Ina219Adc::Adc12_1,
            0b1001 => Ina219Adc::Adc12_2,
            0b1010 => Ina219Adc::Adc12_4,
            0b1011 => Ina219Adc::Adc12_8,
            0b1100 => Ina219Adc::Adc12_16,
            0b1101 => Ina219Adc::Adc12_32,
            0b1110 => Ina219Adc::Adc12_64,
            0b1111 => Ina219Adc::Adc12_128,
            _ => panic!("Invalid ADC Value"), // Not reachable
        }
    }
    pub fn as_cmd(&self) -> [u8; 3] {
        let [b1, b2] = self.0.to_be_bytes();
        [INA219_CONFIG, b1, b2]
    }
    pub fn as_str(&self) -> (&'static str, &'static str, &'static str, &'static str) {
        (
            self.get_brng().as_str(),
            self.get_pga().as_str(),
            self.get_badc().as_str(),
            self.get_sadc().as_str(),
        )
    }
}

#[inline]
fn set_bits(value: u16, field: u16, offset: u8, width: u8) -> u16 {
    let mask = ((1u16 << width) - 1) << offset;
    (value & !mask) | ((field & (mask >> offset)) << offset)
}

#[inline]
fn get_bits(value: u16, offset: u8, width: u8) -> u16 {
    let mask = (1u16 << width) - 1;
    (value >> offset) & mask
}

pub struct Ina219<'a, M, BUS>
where
    M: RawMutex,
    BUS: I2c,
{
    i2c: I2cDevice<'a, M, BUS>,
    address: u8,
    shunt_resistor: f32,
    config: Ina219Config,
}

impl<'a, M, BUS> Ina219<'a, M, BUS>
where
    M: RawMutex,
    BUS: I2c,
{
    pub fn new(i2c: I2cDevice<'a, M, BUS>, address: u8, shunt_resistor: f32) -> Self {
        // Need to reset device before use
        Self {
            i2c,
            address,
            shunt_resistor,
            config: Ina219Config::default(),
        }
    }
    pub async fn reset(&mut self) -> Result<(), Ina219Error> {
        self.i2c
            .write(self.address, &[0x00, 0x80, 0x00])
            .await
            .map_err(|_| Ina219Error::I2cError)?;
        self.config = Ina219Config::default();
        Ok(())
    }
    pub async fn read_config(&mut self) -> Result<Ina219Config, Ina219Error> {
        let mut buf = [0u8; 2];
        self.i2c
            .write_read(self.address, &[INA219_CONFIG], &mut buf[..])
            .await
            .map_err(|_| Ina219Error::I2cError)?;
        self.config = Ina219Config(u16::from_be_bytes(buf));
        Ok(self.config.clone())
    }
    pub async fn write_config(&mut self, config: Ina219Config) -> Result<(), Ina219Error> {
        self.i2c
            .write(self.address, &config.as_cmd())
            .await
            .map_err(|_| Ina219Error::I2cError)?;
        self.config = config;
        Ok(())
    }
    pub async fn read(&mut self) -> Result<Ina219Reading, Ina219Error> {
        let mut buf = [0u8; 2];
        // Bus Voltage
        self.i2c
            .write_read(self.address, &[INA219_BUS_V], &mut buf[..])
            .await
            .map_err(|_| Ina219Error::I2cError)?;
        // defmt::info!( "BUS_V: {} {:016b}", u16::from_be_bytes(buf), u16::from_be_bytes(buf));
        if (buf[1] & 0x01) != 0 {
            // Overflow
            return Err(Ina219Error::Overflow);
        }
        if (buf[1] & 0x02) == 0 {
            // Not Ready
            return Err(Ina219Error::NotReady);
        }
        let bus_v = (u16::from_be_bytes(buf) >> 3) as f32 * 0.004; // LSB = 4mV

        // Shunt Voltage
        self.i2c
            .write_read(self.address, &[INA219_SHUNT_V], &mut buf[..])
            .await
            .map_err(|_| Ina219Error::I2cError)?;

        let shunt_mv = i16::from_be_bytes(buf) as f32 / 100.0;
        // defmt::info!("SHUNT_V: {} {:016b}", shunt_mv, u16::from_be_bytes(buf));
        let shunt_ma = shunt_mv / self.shunt_resistor;

        Ok(Ina219Reading { bus_v, shunt_ma })
    }
}
