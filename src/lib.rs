/*!
This crate provides traits and an adapter for creating move-oriented sequence
and map accessors, as a complement to the remaining serde deserializer traits,
which are entirely move-oriented.

It provides [`SeqAccess`], which replaces [`serde::de::SeqAccess`], and
[`MapKeyAccess`] / [`MapValueAccess`], which collectively replace
[`serde::de::MapAccess`]. These traits are often easier to use when
implementing a [`Deserializer`][serde::de::Deserializer] if you're focused on
using by-move constructs, and help ensure correctness for callers.

In order to interoperate with serde, it also provides [`AccessAdapter`]. This
struct takes any [`SeqAccess`] or [`MapKeyAccess`] type and converts it into
a [`serde::de::SeqAccess`] or [`serde::de::MapAccess`].

# Example

In this example, we're interested in creating a deserializer that reads from
an iterator of `(key, value)` pairs and emits them as a map. We create a a
`KeyAccess` type, which implements [`MapKeyAccess`]. We use `serde-mobile`'s
built-in [`SubordinateValue`] type as our [`MapValueAccess`], because we'll
be using the common pattern where the iterator is stored by both the key and
value accessor. This ends up being easier (if more verbose) to implement: a
[`serde::de::MapAccess`] is a single type that needs to deserialize keys and
values separately, and therefore needs some awkward design to capture the state
where a key has been yielded but not a value. `serde-mobile`, on the other
hand, splits this into a pair of types, so that only correct states can be
expressed.
```
use std::collections::hash_map::{HashMap, IntoIter};
use std::marker::PhantomData;
use serde::de::{
    IntoDeserializer,
    Error,
    DeserializeSeed,
    value::{MapAccessDeserializer, Error as SimpleError},
};
use serde::Deserialize;
use serde_mobile::{
    MapKeyAccess,
    MapValueAccess,
    AccessAdapter,
    SubordinateValue
};

struct KeyAccess<I, E>{
    entries: I,
    error: PhantomData<E>,
}

impl<I, K, V, E> KeyAccess<I, E>
where
    I: Iterator<Item=(K, V)>
{
    fn new<C>(collection: C) -> Self
        where C: IntoIterator<IntoIter=I>
    {
        Self {
            entries: collection.into_iter(),
            error: PhantomData,
        }
    }
}

// MapKeyAccess is the key-getting equivalent of serde::de::MapAccess
impl<'de, I, K, V, E> MapKeyAccess<'de> for KeyAccess<I, E>
where
    I: Iterator<Item=(K, V)>,
    K: IntoDeserializer<'de, E>,
    V: IntoDeserializer<'de, E>,
    E: Error,
{
    type Error = E;
    type Value = SubordinateValue<V::Deserializer, Self>;

    // notice that next_key_seed takes self by move and returns Self::Value,
    // which is a MapKeyAccess. This forces the caller to get a value before
    // they can get another key.
    fn next_key_seed<S>(mut self, seed: S) -> Result<Option<(S::Value, Self::Value)>, Self::Error>
    where
        S: DeserializeSeed<'de>
    {
        self.entries
            .next()
            .map(|(key, value)| {
                seed
                    .deserialize(key.into_deserializer())
                    .map(|key| (
                        key,
                        SubordinateValue {
                            value: value.into_deserializer(),
                            parent: self
                        }
                    ))
            })
            .transpose()
    }

    fn size_hint(&self) -> Option<usize> {
        match self.entries.size_hint() {
            (min, Some(max)) if min == max => Some(min),
            _ => None,
        }
    }
}

// Normally we'd have to create a separate struct to implement `MapValueAccess`,
// but this pattern is common enough that serde-mobile provides a type called
// `SubordinateValue` that handles this pattern for us.

let serialized = HashMap::from([
    ("a", 10),
    ("b", 20),
]);

#[derive(Deserialize, Debug, PartialEq, Eq)]
struct Data {
    a: i32,
    b: i32,
}

let seq_access = KeyAccess::new(serialized);

// use an AccessAdapter to turn a serde-mobile access type
// into a serde access type
let deserializer = MapAccessDeserializer::new(AccessAdapter::new(seq_access));

match Data::deserialize(deserializer) {
    Ok(data) => assert_eq!(data, Data { a: 10, b: 20 }),
    Err(err) => {
        let err: SimpleError = err;
        panic!("failed to deserialize")
    }
}
```
*/

#![no_std]
#![deny(missing_docs)]

// mod visitor;

use core::{convert::Infallible, marker::PhantomData, mem};

use serde::de;

/**
Move-oriented version of [`serde::de::SeqAccess`].

`next_value_seed`  takes `self` by move, and returns an `Option<(S::Value,
Self)>`, ensuring that it's only possible to get another value if a previous
call didn't return `None`.

This trait otherwise is identical to its serde counterpart and serves the same
purpose; see those docs for details.

Use [`AccessAdapter`] to convert this type to a `serde::de::SeqAccess` to pass
into [`visit_seq`][serde::de::Visitor::visit_seq].
*/
pub trait SeqAccess<'de>: Sized {
    /**
    The type that can be returned if an error occurs during deserialization.
    */
    type Error: de::Error;

    /**
    This returns `Ok(Some((value, access)))` for the next value in the
    sequence, or `Ok(None)` if there are no more remaining items. The
    returned `access` value can be used to read additional items from the
    sequence.

    The seed allows for passing data into a `Deserialize` implementation
    at runtime; typically it makes more sense to call [`next_element`][Self::next_element].
    */
    fn next_element_seed<S>(self, seed: S) -> Result<Option<(S::Value, Self)>, Self::Error>
    where
        S: de::DeserializeSeed<'de>;

    /**
    This returns `Ok(Some((value, access)))` for the next value in the
    sequence, or `Ok(None)` if there are no more remaining items. The
    returned `access` value can be used to read additional items from the
    sequence.
    */
    #[inline]
    fn next_element<T>(self) -> Result<Option<(T, Self)>, Self::Error>
    where
        T: de::Deserialize<'de>,
    {
        self.next_element_seed(PhantomData)
    }

    /**
    Returns the number of elements remaining in the sequence, if known.
    */
    #[inline]
    fn size_hint(&self) -> Option<usize> {
        None
    }
}

/**
Move-oriented version of [`serde::de::MapAccess`], for getting keys.

This trait gets around a lot of the shortcomings of serde's `MapAccess` by
using a move-oriented interface:

- It's not possible to call [`next_key`][Self::next_key] after a previous call
  to `next_key` returned `Ok(None)`.
- Accessing the value associated with a key is moved to a different trait,
  which is returned from [`next_key`][Self::next_key]. By splitting key-access
  and value-access into different traits, we can ensure that `next_key` and
  [`next_value`][MapValueAccess::next_value] can never be called out of order,
  removing the need for potential panics (and the associated state tracking
  between keys and values).

This trait otherwise is identical to its serde counterpart and serves the same
purpose; see those docs for details (though note that accessing values is
handled separately through the [`MapValueAccess`] trait.)

Use [`AccessAdapter`] to convert this type to a `serde::de::SeqAccess` to pass
into [`visit_seq`][serde::de::Visitor::visit_seq].
*/
pub trait MapKeyAccess<'de>: Sized {
    /**
    The type that can be returned if an error occurs during deserialization.
    */
    type Error: de::Error;

    /**
    The value accessor associated with this key accessor. A call to
    [`next_key`][Self::next_key] that returns a key will also return a `Value`
    object, which can be used to retrieve the value associated with that key.
    */
    type Value: MapValueAccess<'de, Error = Self::Error, Key = Self>;

    /**
    This returns `Ok(Some((key, value_access)))` for the next key in the
    map, or `Ok(None)` if there are no more entries. The returned
    `value_access` object can be used to retrieve the value associated
    with this key.

    The seed allows for passing data into a `Deserialize` implementation
    at runtime; typically it makes more sense to call [`next_key`][Self::next_key].
    */
    #[allow(clippy::type_complexity)]
    fn next_key_seed<S>(self, seed: S) -> Result<Option<(S::Value, Self::Value)>, Self::Error>
    where
        S: de::DeserializeSeed<'de>;

    /**
    This returns `Ok(Some((key, value_access)))` for the next key in the
    map, or `Ok(None)` if there are no more entries. The returned
    `value_access` can be used to retrieve the value associated with this
    key.
    */
    #[inline]
    fn next_key<T>(self) -> Result<Option<(T, Self::Value)>, Self::Error>
    where
        T: de::Deserialize<'de>,
    {
        self.next_key_seed(PhantomData)
    }

    /**
    This convenience method returns the Ok(Some(((key, value), access)))
    for the next key-value pair in the map, or Ok(None) of there are no
    remaining entries. The returned `access` object can be used to retrieve
    the next entry in the map.

    By default this calls [`next_key_seed`][Self::next_key_seed] followed by
    [`next_value_seed`][MapValueAccess::next_value_seed]; implementors
    should override it if a more efficient implementation is possible.

    The seed allows for passing data into a `Deserialize` implementation
    at runtime; typically it makes more sense to call [`next_entry`][Self::next_entry].
    */
    #[allow(clippy::type_complexity)]
    fn next_entry_seed<K, V>(
        self,
        kseed: K,
        vseed: V,
    ) -> Result<Option<((K::Value, V::Value), Self)>, Self::Error>
    where
        K: de::DeserializeSeed<'de>,
        V: de::DeserializeSeed<'de>,
    {
        self.next_key_seed(kseed)?
            .map(|(key, value_access)| {
                value_access
                    .next_value_seed(vseed)
                    .map(|(value, key_access)| ((key, value), key_access))
            })
            .transpose()
    }

    /**
    This convenience method returns the Ok(Some(((key, value), access)))
    for the next key-value pair in the map, or Ok(None) of there are no
    remaining entries. The returned `access` object can be used to retrieve
    the next entry in the map.
    */
    #[inline]
    #[allow(clippy::type_complexity)]
    fn next_entry<K, V>(
        self,
    ) -> Result<Option<((K, V), <Self::Value as MapValueAccess<'de>>::Key)>, Self::Error>
    where
        K: de::Deserialize<'de>,
        V: de::Deserialize<'de>,
        Self::Value: MapValueAccess<'de, Error = Self::Error>,
    {
        self.next_entry_seed(PhantomData, PhantomData)
    }

    /**
    Returns the number of entries remaining in the map, if known.
    */
    #[inline]
    fn size_hint(&self) -> Option<usize> {
        None
    }
}

/**
Move-oriented version of [`serde::de::MapAccess`] for getting values associated
with keys.

A `MapValueAccess` object is returned alongside a `key` from a call to
[`next_key`][MapKeyAccess::next_key], if the map wasn't empty. It can be used
to retrieve the value associated with that key, and additionally returns a
new [`MapKeyAccess`] that can be used to retrieve the next entry from the map.

This trait otherwise is identical to its serde counterpart and serves the same
purpose; see those docs for details (though note that accessing keys and entries
is handled separately through the [`MapKeyAccess`] trait.)
*/
pub trait MapValueAccess<'de>: Sized {
    /**
    The type that can be returned if an error occurs during deserialization.
    */
    type Error: de::Error;

    /**
    The value accessor associated with this key accessor. A call to
    [`next_value`][Self::next_value] will also return a `Key` object, which can
    be used to retrieve the next key from the map.
    */
    type Key: MapKeyAccess<'de, Error = Self::Error, Value = Self>;

    /**
    This returns the next `value` from the map, associated with the
    previously accessed key. It additionally returns a `key_access` object
    which can be used to retrieve additional keys from the map.

    The seed allows for passing data into a `Deserialize` implementation
    at runtime; typically it makes more sense to call [`next_value`][Self::next_value].
    */
    fn next_value_seed<S>(self, seed: S) -> Result<(S::Value, Self::Key), Self::Error>
    where
        S: de::DeserializeSeed<'de>;

    /**
    This returns the next `value` from the map, associated with the
    previously accessed key. It additionally returns a `key_access` object
    which can be used to retrieve additional keys from the map.
    */
    #[inline]
    fn next_value<T>(self) -> Result<(T, Self::Key), Self::Error>
    where
        T: de::Deserialize<'de>,
    {
        self.next_value_seed(PhantomData)
    }

    /**
    Returns the number of entries remaining in the map, if known. Note that,
    because this returns the number of *entries*, the value associated with
    this particular `MapValueAccess` should not be included.
    */
    #[inline]
    fn size_hint(&self) -> Option<usize> {
        None
    }
}

/**
Adapter type for converting the `serde-mobile` traits into serde's `&mut self`
oriented traits. It uses an internal enum to track the state of the accessors.

Because the underlying traits use a move-oriented interface to prevent calls
after returning `None`, this object is guaranteed to be "fused", meaning that
`next_key` or `next_element` will always continue return `None` when the
underlying collection is exhausted.

This type can be used for both [`serde::de::SeqAccess`] and
[`serde::de::MapAccess`].
*/
#[derive(Debug, Clone)]
pub struct AccessAdapter<K, V> {
    state: AccessAdapterState<K, V>,
}

#[derive(Debug, Clone)]
enum AccessAdapterState<K, V> {
    Ready(K),
    Value(V),
    Dead,
}

impl<K, V> AccessAdapterState<K, V> {
    fn take(&mut self) -> Self {
        mem::replace(self, AccessAdapterState::Dead)
    }
}

impl<T, V> AccessAdapter<T, V> {
    /**
    Create a new `AccessAdapter`, suitable for use as the argument to
    [`visit_seq`][de::Visitor::visit_seq] or [`visit_map`][de::Visitor::visit_map].
    This should be created using a [`SeqAccess`] or [`MapKeyAccess`] object,
    respectively.
    */
    pub fn new(access: T) -> Self {
        Self {
            state: AccessAdapterState::Ready(access),
        }
    }
}

/**
Implementation of [`serde::de::SeqAccess`], using [`SeqAccess`]. An internal
enum is used to track the
*/
impl<'de, S> de::SeqAccess<'de> for AccessAdapter<S, Infallible>
where
    S: SeqAccess<'de>,
{
    type Error = S::Error;

    fn next_element_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>, Self::Error>
    where
        T: de::DeserializeSeed<'de>,
    {
        match self.state.take() {
            AccessAdapterState::Ready(seq) => seq.next_element_seed(seed).map(|opt| {
                opt.map(|(value, seq)| {
                    self.state = AccessAdapterState::Ready(seq);
                    value
                })
            }),
            AccessAdapterState::Value(inf) => match inf {},
            AccessAdapterState::Dead => Ok(None),
        }
    }

    fn size_hint(&self) -> Option<usize> {
        match self.state {
            AccessAdapterState::Ready(ref seq) => seq.size_hint(),
            AccessAdapterState::Value(inf) => match inf {},
            AccessAdapterState::Dead => Some(0),
        }
    }
}

impl<'de, A> de::MapAccess<'de> for AccessAdapter<A, A::Value>
where
    A: MapKeyAccess<'de>,
{
    type Error = A::Error;

    #[inline]
    fn next_key_seed<S>(&mut self, seed: S) -> Result<Option<S::Value>, Self::Error>
    where
        S: de::DeserializeSeed<'de>,
    {
        match self.state.take() {
            AccessAdapterState::Ready(access) => access.next_key_seed(seed).map(|opt| {
                opt.map(|(key, value_access)| {
                    self.state = AccessAdapterState::Value(value_access);
                    key
                })
            }),
            AccessAdapterState::Value(_access) => panic!("called next_key_seed out of order"),
            AccessAdapterState::Dead => Ok(None),
        }
    }

    #[inline]
    fn next_value_seed<S>(&mut self, seed: S) -> Result<S::Value, Self::Error>
    where
        S: de::DeserializeSeed<'de>,
    {
        match self.state.take() {
            AccessAdapterState::Ready(_access) => panic!("called next_value_seed out of order"),
            AccessAdapterState::Value(access) => {
                access.next_value_seed(seed).map(|(value, key_access)| {
                    self.state = AccessAdapterState::Ready(key_access);
                    value
                })
            }
            AccessAdapterState::Dead => panic!("called next_value_seed out of order"),
        }
    }

    fn next_entry_seed<K, V>(
        &mut self,
        key: K,
        value: V,
    ) -> Result<Option<(K::Value, V::Value)>, Self::Error>
    where
        K: de::DeserializeSeed<'de>,
        V: de::DeserializeSeed<'de>,
    {
        match self.state.take() {
            AccessAdapterState::Ready(access) => access.next_entry_seed(key, value).map(|opt| {
                opt.map(|(entry, access)| {
                    self.state = AccessAdapterState::Ready(access);
                    entry
                })
            }),
            AccessAdapterState::Value(_access) => panic!("called next_entry_seed out of order"),
            AccessAdapterState::Dead => Ok(None),
        }
    }

    #[inline]
    fn size_hint(&self) -> Option<usize> {
        match self.state {
            AccessAdapterState::Ready(ref access) => access.size_hint(),
            AccessAdapterState::Value(ref access) => access.size_hint(),
            AccessAdapterState::Dead => Some(0),
        }
    }
}

/// Utility type implementing [`MapValueAccess`] for the common case where a
/// value-access type consists only of a deserializable value, along with the
/// original [`MapKeyAccess`] type that produced it, which will be returned
/// after the value is consumed.
///
/// See the [crate docs][crate] for an example.
#[derive(Debug, Clone)]
pub struct SubordinateValue<V, K> {
    /// The value that will be deserialized in `next_value_seed`.
    pub value: V,

    /// The [`MapKeyAccess`] object that will be returned along with `value`,
    /// after it was deserialized. Usually this will be the same object that
    /// created this [`SubordinateValue`] in the first place.
    pub parent: K,
}

impl<'de, K, V> MapValueAccess<'de> for SubordinateValue<V, K>
where
    V: de::Deserializer<'de>,
    K: MapKeyAccess<'de, Value = Self, Error = V::Error>,
{
    type Error = V::Error;
    type Key = K;

    fn next_value_seed<S>(self, seed: S) -> Result<(S::Value, Self::Key), Self::Error>
    where
        S: de::DeserializeSeed<'de>,
    {
        seed.deserialize(self.value)
            .map(|value| (value, self.parent))
    }

    fn size_hint(&self) -> Option<usize> {
        self.parent.size_hint()
    }
}
