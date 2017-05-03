//! Reader for non-utf8 text.

#![warn(missing_docs)]

extern crate encoding;
extern crate memchr;

use std::{io, result};
use std::borrow::Cow;
use std::io::{BufReader, ErrorKind, Read};
use std::iter::Iterator;

use encoding::{DecoderTrap, Encoding, RawDecoder};
use memchr::memchr;

/// Error for reader.
#[derive(Debug)]
pub enum Error {
    /// IO Error.
    IOError(io::Error),
    /// Encoding error.
    CodecError(Cow<'static, str>)
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Error {
        Error::IOError(err)
    }
}

impl From<Cow<'static, str>> for Error {
    fn from(err: Cow<'static, str>) -> Error {
        Error::CodecError(err)
    }
}

impl From<encoding::CodecError> for Error {
    fn from(err: encoding::CodecError) -> Error {
        Error::CodecError(err.cause)
    }
}

/// Result for reader.
pub type Result<T> = result::Result<T, Error>;

const CHUNK_SIZE: usize = 2048;
const ERR_INCOMPLETE_SEQ: &'static str = "incomplete sequence";

/// The `TextReader` struct is wrapper for `BufReader` to decode text codecs.
pub struct TextReader<R: Read> {
    bufreader: BufReader<R>,
    decoder: Box<RawDecoder>,
    trap: DecoderTrap,
    textbuf: String,
    textbuf_completeseq: bool,
    binbuf: Vec<u8>,
}

impl<R: Read> TextReader<R> {
    /// Creates a new `TextReader` with `codec`.
    ///
    /// # Examples
    /// ```
    /// extern crate textstream;
    /// extern crate encoding;
    /// use std::fs::File;
    /// use encoding::label::encoding_from_whatwg_label;
    /// use encoding::{DecoderTrap, Encoding};
    /// use textstream::TextReader;
    /// # fn foo() -> std::io::Result<()> {
    /// let mut f = File::open("shiftjis.txt")?;
    /// let mut reader = TextReader::new(f, encoding_from_whatwg_label("shiftjis").unwrap(), DecoderTrap::Strict);
    /// # Ok(())
    /// # }
    /// # fn main() { foo(); }
    /// ```
    pub fn new(bufreader: R, encoding: &Encoding, trap: DecoderTrap) -> TextReader<R> {
        TextReader::from_bufreader(BufReader::new(bufreader), encoding, trap)
    }

    /// Creates a new `TextReader` from BufReader.
    ///
    /// # Examples
    /// ```
    /// extern crate textstream;
    /// extern crate encoding;
    /// use std::fs::File;
    /// use std::io::BufReader;
    /// use encoding::label::encoding_from_whatwg_label;
    /// use encoding::{DecoderTrap, Encoding};
    /// use textstream::TextReader;
    /// # fn foo() -> std::io::Result<()> {
    /// let mut f = BufReader::new(File::open("shiftjis.txt")?);
    /// let mut reader = TextReader::new(f, encoding_from_whatwg_label("shiftjis").unwrap(), DecoderTrap::Strict);
    /// # Ok(())
    /// # }
    /// # fn main() { foo(); }
    /// ```
    pub fn from_bufreader(bufreader: BufReader<R>, encoding: &Encoding, trap: DecoderTrap) -> TextReader<R> {
        TextReader {
            bufreader: bufreader,
            decoder: encoding.raw_decoder(),
            trap: trap,
            textbuf: String::new(),
            textbuf_completeseq: true,
            binbuf: Vec::with_capacity(CHUNK_SIZE),
        }
    }

    /// Gets a reference to the underlying text reader.
    /// It is inadvisable to directly read from the underlying reader.
    pub fn get_bufreader(&self) -> &BufReader<R> { &self.bufreader }

    /// Gets a mutable reference to the underlying text reader.
    /// It is inadvisable to directly read from the underlying reader.
    pub fn get_bufreader_mut(&mut self) -> &BufReader<R> { &mut self.bufreader }

    /// Unwraps this `TextReader`, returning the underlying reader.
    /// Note that any leftover data in the internal chunk is lost.
    pub fn into_bufreader(self) -> BufReader<R> { self.bufreader }

    /// Gets a reference to the underlying decoder.
    pub fn get_decoder(&self) -> &RawDecoder { self.decoder.as_ref() }

    /// Gets a mutable reference to the underlying decoder.
    pub fn get_decoder_mut(&mut self) -> &mut RawDecoder { self.decoder.as_mut() }

    /// Unwraps this `TextReader`, returning the underlying decoder.
    pub fn into_decoder(self) -> Box<RawDecoder> { self.decoder }

    /// For internal use. If sequence is incomplete, return false.
    fn _read(&mut self, s: &mut String) -> Result<bool> {
        if self.textbuf.len() > 0 {
            s.push_str(self.textbuf.as_ref());
            let complete = self.textbuf_completeseq;
            self.textbuf.clear();
            self.textbuf_completeseq = true;
            return Ok(complete);
        }
        if self.binbuf.len() < CHUNK_SIZE {
            let mut binbuflen = self.binbuf.len();
            self.binbuf.resize(CHUNK_SIZE, 0);
            let nread = self.bufreader.read(&mut self.binbuf[binbuflen..])?;
            binbuflen += nread;
            self.binbuf.truncate(binbuflen);
        }
        s.reserve(self.binbuf.len());
        let (offset, err) = self.decoder.raw_feed(&self.binbuf[..], s);
        if offset > 0 {
            if offset < self.binbuf.len() {
                self.binbuf = self.binbuf[offset..].to_vec();
            }
            else {
                self.binbuf.clear();
            }
        }
        if let Some(e) = err {
            assert!(e.upto >= offset as isize);
            if !self.trap.trap(&mut *self.decoder, &self.binbuf[..e.upto as usize], s) {
                return Err(Error::from(e.cause));
            }
            if e.upto as usize - offset > 0 {
                self.binbuf = self.binbuf[e.upto as usize - offset..].to_vec();
            }
        }
        let mut is_completeseq = true;
        if let Some(e) = self.decoder.raw_finish(s) {
            if e.cause == ERR_INCOMPLETE_SEQ {
                is_completeseq = false;
            }
            else if !self.trap.trap(&mut *self.decoder, &self.binbuf[..e.upto as usize], s) {
                assert!(e.upto >= 0);
                if e.upto > 0 {
                    self.binbuf = self.binbuf[e.upto as usize - offset..].to_vec();
                }
                return Err(Error::from(e.cause));
            }
        }
        Ok(is_completeseq)
    }

    /// Read decoded text until file end, placing them into `buf`.
    /// If successful, this function will return the total number of bytes read.
    ///
    /// # Examples:
    /// ```
    /// extern crate textstream;
    /// extern crate encoding;
    /// use std::fs::File;
    /// use std::io::BufReader;
    /// use textstream::TextReader;
    /// use encoding::label::encoding_from_whatwg_label;
    /// use encoding::{DecoderTrap, Encoding};
    /// # fn foo() -> textstream::Result<()> {
    /// let mut f = BufReader::new(File::open("shiftjis.txt")?);
    /// let mut reader = TextReader::new(f, encoding_from_whatwg_label("shiftjis").unwrap(), DecoderTrap::Strict);
    /// let mut s = String::new();
    /// reader.read_to_end(&mut s)?;
    /// # Ok(())
    /// # }
    /// # fn main() { foo(); }
    /// ```
    pub fn read_to_end(&mut self, buf: &mut String) -> Result<usize> {
        let nstrlen = buf.len();
        let mut lastlen = buf.len();
        loop {
            match self._read(buf) {
                Err(Error::IOError(ref e)) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => { return Err(e); }
                Ok(complete) => {
                    if buf.len() == lastlen {
                        if complete {
                            return Ok(lastlen - nstrlen);
                        }
                        else {
                            return Err(Error::CodecError(Cow::from(ERR_INCOMPLETE_SEQ)));
                        }
                    }
                    lastlen = buf.len();
                }
            }
        }
    }

    /// Read decoded text until file end, placing them into `buf`.
    /// If successful, this function will return the total number of bytes read.
    ///
    /// # Examples:
    /// ```
    /// extern crate textstream;
    /// extern crate encoding;
    /// use std::fs::File;
    /// use std::io::BufReader;
    /// use encoding::label::encoding_from_whatwg_label;
    /// use encoding::{DecoderTrap, Encoding};
    /// use textstream::TextReader;
    /// # fn foo() -> textstream::Result<()> {
    /// let mut f = BufReader::new(File::open("shiftjis.txt")?);
    /// let mut reader = TextReader::new(f, encoding_from_whatwg_label("shiftjis").unwrap(), DecoderTrap::Strict);
    /// let mut s = String::new();
    /// reader.read_line(&mut s)?;
    /// # Ok(())
    /// # }
    /// # fn main() { foo(); }
    /// ```
    pub fn read_line(&mut self, buf: &mut String) -> Result<usize> {
        let nstrlen = buf.len();
        let mut lastlen = buf.len();
        loop {
            let result = self._read(buf);
            let newlen = buf.len();
            match memchr(b'\n', &buf[lastlen..].as_bytes()) {
                Some(n) => {
                    if lastlen + n + 1 < newlen {
                        self.textbuf = buf[lastlen + n + 1..].to_string();
                        self.textbuf_completeseq = match result.as_ref() {
                            Err(&Error::CodecError(ref s)) if s == ERR_INCOMPLETE_SEQ => true,
                            _ => false
                        };
                        buf.truncate(lastlen + n + 1);
                    }
                    return Ok(lastlen + n + 1 - nstrlen);
                },
                _ => {}
            }
            match result {
                Err(e) => {
                    match e {
                        Error::IOError(ref ioerr) if ioerr.kind() == ErrorKind::Interrupted => {
                            lastlen = newlen;
                            continue;
                        },
                        Error::IOError(ref ioerr) if ioerr.kind() == ErrorKind::UnexpectedEof => {
                            return Ok(newlen - nstrlen);
                        },
                        _ => return Err(e),
                    }
                }
                _ => {}
            }
            if lastlen == newlen {
                return Ok(newlen - nstrlen);
            }
            lastlen = newlen;
        }
    }

    /// Returns an iterator over the lines of this reader.
    /// The iterator returned from this function will yield instances of
    /// `textstream::Result<String>`. Each string will not have a newline byte (the 0xA byte) or
    /// CRLF (0xD, 0xA bytes) at the end.
    pub fn lines(self) -> Lines<R> {
        Lines { textreader: self }
    }
}

/// An iterator over the lines of an `TextReader`.
/// This struct is generally created by calling `lines()` on a `TextReader`. Please see the
/// documentation of `lines()` for more details.
pub struct Lines<R: Read> {
    textreader: TextReader<R>
}
impl<R: Read> Iterator for Lines<R> {
    type Item = Result<String>;
    fn next(&mut self) -> Option<Self::Item> {
        let mut s = String::new();
        match self.textreader.read_line(&mut s) {
            Ok(_) => {
                if s.len() > 0 {
                    if s.ends_with("\n") {
                        s.pop();
                        if s.ends_with("\r") {
                            s.pop();
                        }
                    }
                    Some(Ok(s))
                }
                else {
                    None
                }
            },
            Err(e) => {
                Some(Err(e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use encoding::label::encoding_from_whatwg_label;
    use encoding::DecoderTrap;

    #[test]
    fn read_to_end_shortstring() {
        let sjis_aiueo = [0x82, 0xa0, 0x82, 0xa2, 0x82, 0xa4, 0x82, 0xa6, 0x82, 0xa8];
        let mut reader = TextReader::new(&sjis_aiueo[..], encoding_from_whatwg_label("sjis").unwrap(), DecoderTrap::Strict);
        let mut s = String::new();
        assert!(reader.read_to_end(&mut s).is_ok());
        assert_eq!(s.as_str(), "あいうえお");
    }
    #[test]
    fn read_to_end_longstring() {
        let sjis_aiueo = [0x82, 0xa0, 0x82, 0xa2, 0x82, 0xa4, 0x82, 0xa6, 0x82, 0xa8];
        let mut v = vec![0x41u8];
        let mut s_answer = "A".to_string();
        for _ in 0..300 {
            v.extend_from_slice(&sjis_aiueo);
            s_answer += "あいうえお";
        }
        let mut reader = TextReader::new(&v[..], encoding_from_whatwg_label("sjis").unwrap(), DecoderTrap::Strict);
        let mut s = String::new();
        assert!(reader.read_to_end(&mut s).is_ok());
        assert_eq!(s.len(), s_answer.len());
        assert_eq!(s, s_answer);
    }
    #[test]
    fn read_line_shortstring() {
        let sjis_aiueo = [0x82, 0xa0, 0x82, 0xa2, 0x82, 0xa4, 0x82, 0xa6, 0x82, 0xa8];
        let mut v = vec![];
        v.extend_from_slice(&sjis_aiueo);
        v.push(10);
        v.extend_from_slice(&sjis_aiueo);
        v.push(10);
        v.extend_from_slice(&sjis_aiueo);
        let mut reader = TextReader::new(&v[..], encoding_from_whatwg_label("sjis").unwrap(), DecoderTrap::Strict);
        let mut s = String::new();
        assert!(match reader.read_line(&mut s) { Ok(16usize) => true, _ => false });
        assert_eq!(s, "あいうえお\n");
        assert!(match reader.read_line(&mut s) { Ok(16usize) => true, _ => false });
        assert_eq!(s, "あいうえお\nあいうえお\n");
        s.clear();
        assert!(match reader.read_line(&mut s) { Ok(15usize) => true, _ => false });
        assert_eq!(s, "あいうえお");
    }
    #[test]
    fn read_line_then_read_to_end_shortstring() {
        let sjis_aiueo = [0x82, 0xa0, 0x82, 0xa2, 0x82, 0xa4, 0x82, 0xa6, 0x82, 0xa8];
        let mut v = vec![];
        v.extend_from_slice(&sjis_aiueo);
        v.push(10);
        v.extend_from_slice(&"abcd".as_bytes());
        v.extend_from_slice(&sjis_aiueo);
        let mut reader = TextReader::new(&v[..], encoding_from_whatwg_label("sjis").unwrap(), DecoderTrap::Strict);
        let mut s = String::new();
        assert!(reader.read_line(&mut s).is_ok());
        s.clear();
        assert!(reader.read_to_end(&mut s).is_ok());
        assert_eq!(s, "abcdあいうえお");
    }
    #[test]
    fn lines_test() {
        let sjis_aiueo = [0x82, 0xa0, 0x82, 0xa2, 0x82, 0xa4, 0x82, 0xa6, 0x82, 0xa8];
        let mut v = vec![];
        v.extend_from_slice(&sjis_aiueo);
        v.push(10);
        v.extend_from_slice(&sjis_aiueo);
        v.push(10);
        v.extend_from_slice(&sjis_aiueo);
        let reader = TextReader::new(&v[..], encoding_from_whatwg_label("sjis").unwrap(), DecoderTrap::Strict);
        let mut res: Vec<_> = reader.lines().collect();
        assert_eq!(res.len(), 3);
        assert!(res[0].is_ok());
        assert!(res[1].is_ok());
        assert!(res[2].is_ok());
        assert_eq!(res.pop().unwrap().unwrap(), "あいうえお"); // res[2]
        assert_eq!(res.pop().unwrap().unwrap(), "あいうえお"); // res[1]
        assert_eq!(res.pop().unwrap().unwrap(), "あいうえお"); // res[0]
    }
}
