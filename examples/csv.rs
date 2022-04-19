use std::io;

use anyhow::Context;
use serde::{
    de::{self, IntoDeserializer},
    forward_to_deserialize_any, Deserialize,
};
use thiserror::Error;

fn parse_csv_line<'b>(
    input: &mut impl io::BufRead,
    buffer: &'b mut String,
) -> io::Result<Option<impl Iterator<Item = &'b str>>> {
    buffer.clear();

    input
        .read_line(buffer)
        .map(|_| (!buffer.is_empty()).then(|| buffer.trim_end_matches('\n').split(',')))
}

#[derive(Debug, Error)]
enum Error {
    #[error("io error during deserialization")]
    Io(#[from] io::Error),

    #[error("error from deserialized type: {0}")]
    Custom(String),

    #[error("row and header row had different lengths")]
    HeaderLengthMismatch,

    #[error("there wasn't a header row in the document")]
    NoHeaderRow,
}

impl de::Error for Error {
    fn custom<T>(msg: T) -> Self
    where
        T: std::fmt::Display,
    {
        Self::Custom(msg.to_string())
    }
}

struct Deserializer<R: io::BufRead> {
    input: R,
    headers: bool,
}

impl<'de, R: io::BufRead> de::Deserializer<'de> for Deserializer<R> {
    type Error = Error;

    fn deserialize_any<V>(mut self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        match self.headers {
            false => visitor.visit_seq(serde_mobile::AccessAdapter::new(RowsSeqAccess {
                input: self.input,
                headers: Headerless,
                buffer: String::new(),
            })),
            true => {
                let mut headers_buffer = String::new();

                let headers = parse_csv_line(&mut self.input, &mut headers_buffer)?
                    .ok_or(Error::NoHeaderRow)?
                    .collect();

                visitor.visit_seq(serde_mobile::AccessAdapter::new(RowsSeqAccess {
                    headers: HeaderList { headers },
                    input: self.input,
                    buffer: String::new(),
                }))
            }
        }
    }

    forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf option unit unit_struct newtype_struct seq tuple
        tuple_struct map struct enum identifier ignored_any
    }
}

/// SeqAccess for reading all the rows from a CSV document
struct RowsSeqAccess<R, H> {
    input: R,
    headers: H,
    buffer: String,
}

impl<'de, 'h, R: io::BufRead, H: Headers> serde_mobile::SeqAccess<'de> for RowsSeqAccess<R, H> {
    type Error = Error;

    fn next_element_seed<S>(mut self, seed: S) -> Result<Option<(S::Value, Self)>, Self::Error>
    where
        S: de::DeserializeSeed<'de>,
    {
        // Fetch a row to be deserialized
        let row = match parse_csv_line(&mut self.input, &mut self.buffer)? {
            Some(row) => row,
            None => return Ok(None),
        };

        // Deserialize it
        let row = seed.deserialize(RowDeserializer {
            row,
            headers: &self.headers,
        })?;

        Ok(Some((row, self)))
    }
}

struct RowDeserializer<'a, I, H> {
    row: I,
    headers: &'a H,
}

impl<'de, 'a, I, H> de::Deserializer<'de> for RowDeserializer<'a, I, H>
where
    I: Iterator<Item = &'a str>,
    H: Headers,
{
    type Error = Error;

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        self.deserialize_map(visitor)
    }

    fn deserialize_seq<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        visitor.visit_seq(serde_mobile::AccessAdapter::new(RowSeqAccess {
            row: self.row,
        }))
    }

    fn deserialize_map<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        self.headers.apply_visitor(visitor, self.row)
    }

    forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf option unit unit_struct newtype_struct tuple
        tuple_struct struct enum identifier ignored_any
    }
}

trait Headers {
    fn apply_visitor<'de, 'v, V, I>(&self, visitor: V, row: I) -> Result<V::Value, Error>
    where
        V: de::Visitor<'de>,
        I: Iterator<Item = &'v str>;
}

#[derive(Copy, Clone)]
struct Headerless;

impl Headers for Headerless {
    fn apply_visitor<'de, 'v, V, I>(&self, visitor: V, row: I) -> Result<V::Value, Error>
    where
        V: de::Visitor<'de>,
        I: Iterator<Item = &'v str>,
    {
        visitor.visit_seq(serde_mobile::AccessAdapter::new(RowSeqAccess { row }))
    }
}

struct HeaderList<'a> {
    headers: Vec<&'a str>,
}

impl<'k> Headers for HeaderList<'k> {
    fn apply_visitor<'de, 'a, V, I>(&self, visitor: V, row: I) -> Result<V::Value, Error>
    where
        V: de::Visitor<'de>,
        I: Iterator<Item = &'a str>,
    {
        visitor.visit_map(serde_mobile::AccessAdapter::new(RowMapKeyAccess {
            row,
            headers: &self.headers,
        }))
    }
}

struct RowSeqAccess<I> {
    row: I,
}

impl<'de, 'a, I: Iterator<Item = &'a str>> serde_mobile::SeqAccess<'de> for RowSeqAccess<I> {
    type Error = Error;

    fn next_element_seed<S>(mut self, seed: S) -> Result<Option<(S::Value, Self)>, Self::Error>
    where
        S: de::DeserializeSeed<'de>,
    {
        self.row
            .next()
            .map(|element| {
                seed.deserialize(element.into_deserializer())
                    .map(|value| (value, self))
            })
            .transpose()
    }

    fn size_hint(&self) -> Option<usize> {
        match self.row.size_hint() {
            (min, Some(max)) if min == max => Some(min),
            _ => None,
        }
    }
}

struct RowMapKeyAccess<'k, I> {
    row: I,
    headers: &'k [&'k str],
}

impl<'de, 'k, 'v, I> serde_mobile::MapKeyAccess<'de> for RowMapKeyAccess<'k, I>
where
    I: Iterator<Item = &'v str>,
{
    type Error = Error;

    type Value = RowMapValueAccess<'k, 'v, I>;

    fn next_key_seed<S>(mut self, seed: S) -> Result<Option<(S::Value, Self::Value)>, Self::Error>
    where
        S: de::DeserializeSeed<'de>,
    {
        match (self.headers.split_first(), self.row.next()) {
            (Some((&header, tail)), Some(value)) => {
                let value_access = RowMapValueAccess {
                    value,
                    key_access: RowMapKeyAccess {
                        row: self.row,
                        headers: tail,
                    },
                };

                seed.deserialize(header.into_deserializer())
                    .map(|key| (key, value_access))
                    .map(Some)
            }
            (None, None) => Ok(None),
            _ => Err(Error::HeaderLengthMismatch),
        }
    }
}

struct RowMapValueAccess<'k, 'v, I> {
    value: &'v str,
    key_access: RowMapKeyAccess<'k, I>,
}

impl<'de, 'k, 'v, I> serde_mobile::MapValueAccess<'de> for RowMapValueAccess<'k, 'v, I>
where
    I: Iterator<Item = &'v str>,
{
    type Error = Error;

    type Key = RowMapKeyAccess<'k, I>;

    fn next_value_seed<S>(self, seed: S) -> Result<(S::Value, Option<Self::Key>), Self::Error>
    where
        S: de::DeserializeSeed<'de>,
    {
        seed.deserialize(self.value.into_deserializer())
            .map(|value| (value, Some(self.key_access)))
    }
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct Data {
    name: String,
    description: String,
}

fn main() -> anyhow::Result<()> {
    let stdin = std::io::stdin();
    let stdin = stdin.lock();
    let csv = Deserializer {
        headers: true,
        input: stdin,
    };
    let data: Vec<Data> = Vec::deserialize(csv).context("failed to deserialize")?;
    println!("{:#?}", data);
    Ok(())
}
