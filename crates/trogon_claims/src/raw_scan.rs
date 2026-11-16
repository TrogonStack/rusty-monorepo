pub(crate) struct Scanner<'a> {
    text: &'a str,
    pos: usize,
}

impl<'a> Scanner<'a> {
    pub(crate) fn new(text: &'a str) -> Self {
        Self { text, pos: 0 }
    }

    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.pos).copied()
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, byte: u8) -> Option<()> {
        if self.peek()? == byte {
            self.pos += 1;
            Some(())
        } else {
            None
        }
    }

    fn string_span(&mut self) -> Option<&'a str> {
        let start = self.pos;
        self.expect(b'"')?;
        loop {
            match self.peek()? {
                b'\\' => self.pos += 2,
                b'"' => {
                    self.pos += 1;
                    break;
                }
                _ => self.pos += 1,
            }
        }
        self.text.get(start..self.pos)
    }

    fn value_span(&mut self) -> Option<&'a str> {
        let start = self.pos;
        match self.peek()? {
            b'"' => {
                self.string_span()?;
            }
            b'{' | b'[' => {
                let mut depth = 0usize;
                loop {
                    match self.peek()? {
                        b'"' => {
                            self.string_span()?;
                            continue;
                        }
                        b'{' | b'[' => depth += 1,
                        b'}' | b']' => {
                            depth = depth.checked_sub(1)?;
                            if depth == 0 {
                                self.pos += 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                    self.pos += 1;
                }
            }
            _ => {
                while let Some(byte) = self.peek() {
                    if matches!(byte, b',' | b']' | b'}' | b' ' | b'\t' | b'\r' | b'\n') {
                        break;
                    }
                    self.pos += 1;
                }
            }
        }
        self.text.get(start..self.pos)
    }
}

pub(crate) fn object_members(text: &str) -> Option<Vec<(&str, &str)>> {
    let mut scanner = Scanner::new(text);
    scanner.skip_whitespace();
    scanner.expect(b'{')?;
    scanner.skip_whitespace();
    let mut members = Vec::new();
    if scanner.peek()? == b'}' {
        return Some(members);
    }
    loop {
        scanner.skip_whitespace();
        let key = scanner.string_span()?;
        scanner.skip_whitespace();
        scanner.expect(b':')?;
        scanner.skip_whitespace();
        let value = scanner.value_span()?;
        members.push((key, value));
        scanner.skip_whitespace();
        match scanner.peek()? {
            b',' => scanner.pos += 1,
            b'}' => return Some(members),
            _ => return None,
        }
    }
}

pub(crate) fn array_elements(text: &str) -> Option<Vec<&str>> {
    let mut scanner = Scanner::new(text);
    scanner.skip_whitespace();
    scanner.expect(b'[')?;
    scanner.skip_whitespace();
    let mut elements = Vec::new();
    if scanner.peek()? == b']' {
        return Some(elements);
    }
    loop {
        scanner.skip_whitespace();
        elements.push(scanner.value_span()?);
        scanner.skip_whitespace();
        match scanner.peek()? {
            b',' => scanner.pos += 1,
            b']' => return Some(elements),
            _ => return None,
        }
    }
}
