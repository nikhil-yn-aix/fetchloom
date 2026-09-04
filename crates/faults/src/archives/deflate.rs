//! A DEFLATE encoder that emits the exact bit stream a fixture needs, so a
//! corpus entry is a known byte string rather than whatever a library chose.

struct BitWriter {
    bytes: Vec<u8>,
    current: u8,
    filled: u8,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            current: 0,
            filled: 0,
        }
    }

    fn push_bit(&mut self, bit: u8) {
        self.current |= (bit & 1) << self.filled;
        self.filled += 1;
        if self.filled == 8 {
            self.bytes.push(self.current);
            self.current = 0;
            self.filled = 0;
        }
    }

    fn push_value(&mut self, value: u32, bits: u8) {
        for index in 0..bits {
            self.push_bit(((value >> index) & 1) as u8);
        }
    }

    fn push_code(&mut self, code: u32, bits: u8) {
        for index in (0..bits).rev() {
            self.push_bit(((code >> index) & 1) as u8);
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.filled > 0 {
            self.bytes.push(self.current);
        }
        self.bytes
    }
}

fn fixed_literal_code(byte: u8) -> (u32, u8) {
    let value = u32::from(byte);
    if value <= 143 {
        (0x30 + value, 8)
    } else {
        (0x190 + (value - 144), 9)
    }
}

fn fixed_length_symbol_code(symbol: u32) -> (u32, u8) {
    if symbol == 256 {
        (0, 7)
    } else if symbol <= 279 {
        (1 + (symbol - 257), 7)
    } else {
        (0xC0 + (symbol - 280), 8)
    }
}

pub(super) fn deflate_repeated_byte(byte: u8, count: u32) -> Vec<u8> {
    let mut writer = BitWriter::new();
    writer.push_value(1, 1);
    writer.push_value(1, 2);
    if count == 0 {
        let (end_code, end_bits) = fixed_length_symbol_code(256);
        writer.push_code(end_code, end_bits);
        return writer.finish();
    }
    let (literal_code, literal_bits) = fixed_literal_code(byte);
    writer.push_code(literal_code, literal_bits);
    let mut remaining = count - 1;
    while remaining >= 258 {
        let (length_code, length_bits) = fixed_length_symbol_code(285);
        writer.push_code(length_code, length_bits);
        writer.push_code(0, 5);
        remaining -= 258;
    }
    for _ in 0..remaining {
        let (literal_code, literal_bits) = fixed_literal_code(byte);
        writer.push_code(literal_code, literal_bits);
    }
    let (end_code, end_bits) = fixed_length_symbol_code(256);
    writer.push_code(end_code, end_bits);
    writer.finish()
}
