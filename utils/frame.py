import struct
import serial

def crc16(data: bytes) -> int:
    crc = 0xFFFF
    for byte in data:
        # XOR the byte into the top byte of the crc
        crc ^= (byte << 8)
        for _ in range(8):
            # Check if the most significant bit is set
            if crc & 0x8000:
                # Shift left and XOR with the polynomial 0x1021
                crc = (crc << 1) ^ 0x1021
            else:
                # Just shift left
                crc = crc << 1

            # Mask to keep the value within 16 bits (u16 behavior)
            crc &= 0xFFFF
    return crc

def frame(data: bytes) -> bytes:
    return struct.pack("<BBBBH",0xAA,0xCC,0xAA,0xCC,len(data)) + data + struct.pack("<H",crc16(data))

device = "/dev/tty.usbmodem3101"
ser = serial.Serial(device) 
ser.write(frame(b"ABCDE"))
