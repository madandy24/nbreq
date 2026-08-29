//! Private bounded DNS wire codec.
//!
//! This module deliberately knows nothing about resolver sockets, retries, caches, search policy,
//! cancellation, or Engine lifecycle. The native resolver owns those policies and feeds this codec
//! only bounded packets.

use std::net::{Ipv4Addr, Ipv6Addr};

pub(super) const MAX_PACKET_LEN: usize = 4096;
const DNS_HEADER_LEN: usize = 12;
const MAX_WIRE_NAME_LEN: usize = 255;
const MAX_LABEL_LEN: usize = 63;
const MAX_POINTER_HOPS: usize = 32;
const CLASS_IN: u16 = 1;
const TYPE_CNAME: u16 = 5;
const TYPE_SOA: u16 = 6;
const TYPE_OPT: u16 = 41;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct Name {
    // Labels are stored in case-folded wire form. DNS ASCII letters compare case-insensitively;
    // non-ASCII label octets are retained so malformed presentation text cannot escape the codec.
    labels: Vec<Vec<u8>>,
}

impl Name {
    pub(crate) fn from_ascii(value: &str) -> Result<Self, WireError> {
        if !value.is_ascii() {
            return Err(WireError::new("DNS presentation name is not ASCII"));
        }
        if value == "." {
            return Ok(Self::root());
        }
        let value = value.strip_suffix('.').unwrap_or(value);
        if value.is_empty() {
            return Err(WireError::new("DNS presentation name is empty"));
        }
        let mut labels = Vec::new();
        let mut wire_len = 1_usize;
        for label in value.split('.') {
            if label.is_empty() || label.len() > MAX_LABEL_LEN {
                return Err(WireError::new("DNS presentation label length is invalid"));
            }
            wire_len = wire_len
                .checked_add(label.len() + 1)
                .ok_or_else(|| WireError::new("DNS presentation name length overflowed"))?;
            if wire_len > MAX_WIRE_NAME_LEN {
                return Err(WireError::new("DNS presentation name is too long"));
            }
            labels.push(fold_label(label.as_bytes()));
        }
        Ok(Self { labels })
    }

    pub(crate) fn root() -> Self {
        Self { labels: Vec::new() }
    }

    pub(crate) fn is_root(&self) -> bool {
        self.labels.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn set_fqdn(&mut self, _fqdn: bool) -> &mut Self {
        self
    }

    #[cfg(test)]
    pub(crate) fn to_ascii(&self) -> String {
        if self.is_root() {
            return ".".to_owned();
        }
        let mut output = String::new();
        for (index, label) in self.labels.iter().enumerate() {
            if index > 0 {
                output.push('.');
            }
            for byte in label {
                if byte.is_ascii_graphic() && *byte != b'.' && *byte != b'\\' {
                    output.push(char::from(*byte));
                } else {
                    use std::fmt::Write as _;
                    let _ = write!(output, "\\{:03}", byte);
                }
            }
        }
        output.push('.');
        output
    }

    #[cfg(test)]
    pub(crate) fn to_utf8(&self) -> String {
        self.to_ascii()
    }

    fn encode(&self, output: &mut Vec<u8>) {
        for label in &self.labels {
            output.push(label.len() as u8);
            output.extend_from_slice(label);
        }
        output.push(0);
    }
}

fn fold_label(label: &[u8]) -> Vec<u8> {
    label.iter().map(|byte| byte.to_ascii_lowercase()).collect()
}

#[allow(clippy::upper_case_acronyms)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum RecordType {
    A,
    AAAA,
}

impl RecordType {
    fn code(self) -> u16 {
        match self {
            Self::A => 1,
            Self::AAAA => 28,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Question {
    pub(super) name: Name,
    pub(super) record_type: u16,
    pub(super) class: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum RData {
    A(Ipv4Addr),
    Aaaa(Ipv6Addr),
    Cname(Name),
    Soa { minimum: u32 },
    Opt { extended_rcode: u8 },
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Record {
    pub(super) name: Name,
    pub(super) record_type: u16,
    pub(super) class: u16,
    pub(super) ttl: u32,
    pub(super) data: RData,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Response {
    pub(super) id: u16,
    pub(super) is_response: bool,
    pub(super) truncated: bool,
    pub(super) rcode: u16,
    pub(super) questions: Vec<Question>,
    pub(super) answers: Vec<Record>,
    pub(super) authorities: Vec<Record>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct WireError {
    message: &'static str,
}

impl WireError {
    fn new(message: &'static str) -> Self {
        Self { message }
    }

    #[cfg(test)]
    pub(super) fn message(self) -> &'static str {
        self.message
    }
}

pub(super) fn encode_query(
    id: u16,
    name: &Name,
    record_type: RecordType,
) -> Result<Vec<u8>, WireError> {
    if name.is_root() {
        return Err(WireError::new("DNS query name is the root"));
    }
    let mut output = Vec::with_capacity(DNS_HEADER_LEN + MAX_WIRE_NAME_LEN + 4);
    put_u16(&mut output, id);
    put_u16(&mut output, 0x0100); // recursion desired
    put_u16(&mut output, 1); // one question
    put_u16(&mut output, 0);
    put_u16(&mut output, 0);
    put_u16(&mut output, 0);
    name.encode(&mut output);
    put_u16(&mut output, record_type.code());
    put_u16(&mut output, CLASS_IN);
    if output.len() > MAX_PACKET_LEN {
        return Err(WireError::new("encoded DNS query exceeds the packet bound"));
    }
    Ok(output)
}

pub(super) fn parse_response(bytes: &[u8]) -> Result<Response, WireError> {
    if bytes.len() < DNS_HEADER_LEN || bytes.len() > MAX_PACKET_LEN {
        return Err(WireError::new(
            "DNS response length is outside the packet bound",
        ));
    }
    let mut cursor = 0_usize;
    let id = take_u16(bytes, &mut cursor)?;
    let flags = take_u16(bytes, &mut cursor)?;
    let question_count = usize::from(take_u16(bytes, &mut cursor)?);
    let answer_count = usize::from(take_u16(bytes, &mut cursor)?);
    let authority_count = usize::from(take_u16(bytes, &mut cursor)?);
    let additional_count = usize::from(take_u16(bytes, &mut cursor)?);
    validate_minimum_shape(
        bytes.len() - DNS_HEADER_LEN,
        question_count,
        answer_count,
        authority_count,
        additional_count,
    )?;

    let mut questions = Vec::new();
    for _ in 0..question_count {
        let name = read_name(bytes, &mut cursor)?;
        questions.push(Question {
            name,
            record_type: take_u16(bytes, &mut cursor)?,
            class: take_u16(bytes, &mut cursor)?,
        });
    }

    let mut answers = Vec::new();
    for _ in 0..answer_count {
        answers.push(read_record(bytes, &mut cursor)?);
    }
    let mut authorities = Vec::new();
    for _ in 0..authority_count {
        authorities.push(read_record(bytes, &mut cursor)?);
    }
    let mut extended_rcode = None;
    for _ in 0..additional_count {
        let record = read_record(bytes, &mut cursor)?;
        if let RData::Opt {
            extended_rcode: value,
        } = record.data
        {
            if extended_rcode.replace(value).is_some() {
                return Err(WireError::new("DNS response contains multiple OPT records"));
            }
        }
    }
    if cursor != bytes.len() {
        return Err(WireError::new("DNS response has trailing bytes"));
    }
    let base_rcode = flags & 0x000f;
    let rcode = (u16::from(extended_rcode.unwrap_or(0)) << 4) | base_rcode;
    Ok(Response {
        id,
        is_response: flags & 0x8000 != 0,
        truncated: flags & 0x0200 != 0,
        rcode,
        questions,
        answers,
        authorities,
    })
}

fn validate_minimum_shape(
    remaining: usize,
    questions: usize,
    answers: usize,
    authorities: usize,
    additionals: usize,
) -> Result<(), WireError> {
    // Root question is one name octet plus type/class. A resource record is one root-name octet,
    // its fixed ten-octet header, and possibly empty RDATA. Compression cannot make either less.
    let question_minimum = questions
        .checked_mul(5)
        .ok_or_else(|| WireError::new("DNS question count overflowed"))?;
    let record_count = answers
        .checked_add(authorities)
        .and_then(|count| count.checked_add(additionals))
        .ok_or_else(|| WireError::new("DNS record count overflowed"))?;
    let record_minimum = record_count
        .checked_mul(11)
        .ok_or_else(|| WireError::new("DNS record count overflowed"))?;
    if question_minimum
        .checked_add(record_minimum)
        .is_none_or(|minimum| minimum > remaining)
    {
        return Err(WireError::new(
            "DNS section counts cannot fit in the packet",
        ));
    }
    Ok(())
}

fn read_record(bytes: &[u8], cursor: &mut usize) -> Result<Record, WireError> {
    let name = read_name(bytes, cursor)?;
    let record_type = take_u16(bytes, cursor)?;
    let class = take_u16(bytes, cursor)?;
    let ttl = take_u32(bytes, cursor)?;
    let data_len = usize::from(take_u16(bytes, cursor)?);
    let data_start = *cursor;
    let data_end = data_start
        .checked_add(data_len)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| WireError::new("DNS RDATA extends beyond the packet"))?;

    let data = match record_type {
        1 => {
            if data_len != 4 {
                return Err(WireError::new("DNS A RDATA has the wrong length"));
            }
            RData::A(Ipv4Addr::new(
                bytes[data_start],
                bytes[data_start + 1],
                bytes[data_start + 2],
                bytes[data_start + 3],
            ))
        }
        28 => {
            if data_len != 16 {
                return Err(WireError::new("DNS AAAA RDATA has the wrong length"));
            }
            let mut octets = [0_u8; 16];
            octets.copy_from_slice(&bytes[data_start..data_end]);
            RData::Aaaa(Ipv6Addr::from(octets))
        }
        TYPE_CNAME => {
            let mut name_cursor = data_start;
            let target = read_name(bytes, &mut name_cursor)?;
            if name_cursor != data_end {
                return Err(WireError::new("DNS CNAME RDATA has trailing bytes"));
            }
            RData::Cname(target)
        }
        TYPE_SOA => {
            let mut soa_cursor = data_start;
            let _primary = read_name(bytes, &mut soa_cursor)?;
            let _mailbox = read_name(bytes, &mut soa_cursor)?;
            let _serial = take_u32_bounded(bytes, &mut soa_cursor, data_end)?;
            let _refresh = take_u32_bounded(bytes, &mut soa_cursor, data_end)?;
            let _retry = take_u32_bounded(bytes, &mut soa_cursor, data_end)?;
            let _expire = take_u32_bounded(bytes, &mut soa_cursor, data_end)?;
            let minimum = take_u32_bounded(bytes, &mut soa_cursor, data_end)?;
            if soa_cursor != data_end {
                return Err(WireError::new("DNS SOA RDATA has trailing bytes"));
            }
            RData::Soa { minimum }
        }
        TYPE_OPT => RData::Opt {
            extended_rcode: (ttl >> 24) as u8,
        },
        _ => RData::Other,
    };
    *cursor = data_end;
    Ok(Record {
        name,
        record_type,
        class,
        ttl,
        data,
    })
}

fn read_name(bytes: &[u8], cursor: &mut usize) -> Result<Name, WireError> {
    let mut labels = Vec::new();
    let mut position = *cursor;
    let mut jumped = false;
    let mut pointer_hops = 0_usize;
    let mut visited = Vec::new();
    let mut wire_len = 1_usize;

    loop {
        let length = *bytes
            .get(position)
            .ok_or_else(|| WireError::new("DNS name extends beyond the packet"))?;
        if length & 0xc0 == 0xc0 {
            let low = *bytes
                .get(position + 1)
                .ok_or_else(|| WireError::new("DNS compression pointer is incomplete"))?;
            let target = (usize::from(length & 0x3f) << 8) | usize::from(low);
            if target >= bytes.len() {
                return Err(WireError::new(
                    "DNS compression pointer is outside the packet",
                ));
            }
            if target >= position {
                return Err(WireError::new(
                    "DNS compression pointer does not point backward",
                ));
            }
            if !jumped {
                *cursor = position + 2;
                jumped = true;
            }
            pointer_hops += 1;
            if pointer_hops > MAX_POINTER_HOPS || visited.contains(&target) {
                return Err(WireError::new(
                    "DNS compression pointer loop or depth exceeded",
                ));
            }
            visited.push(target);
            position = target;
            continue;
        }
        if length & 0xc0 != 0 {
            return Err(WireError::new(
                "DNS name uses an unsupported label encoding",
            ));
        }
        position += 1;
        if length == 0 {
            if !jumped {
                *cursor = position;
            }
            return Ok(Name { labels });
        }
        let length = usize::from(length);
        if length > MAX_LABEL_LEN {
            return Err(WireError::new("DNS label exceeds 63 octets"));
        }
        let end = position
            .checked_add(length)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| WireError::new("DNS label extends beyond the packet"))?;
        wire_len = wire_len
            .checked_add(length + 1)
            .ok_or_else(|| WireError::new("decoded DNS name length overflowed"))?;
        if wire_len > MAX_WIRE_NAME_LEN {
            return Err(WireError::new("decoded DNS name exceeds 255 wire octets"));
        }
        labels.push(fold_label(&bytes[position..end]));
        position = end;
    }
}

fn take_u16(bytes: &[u8], cursor: &mut usize) -> Result<u16, WireError> {
    let first = *bytes
        .get(*cursor)
        .ok_or_else(|| WireError::new("DNS field extends beyond the packet"))?;
    let second = *bytes
        .get(*cursor + 1)
        .ok_or_else(|| WireError::new("DNS field extends beyond the packet"))?;
    *cursor += 2;
    Ok(u16::from_be_bytes([first, second]))
}

fn take_u32(bytes: &[u8], cursor: &mut usize) -> Result<u32, WireError> {
    let end = cursor
        .checked_add(4)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| WireError::new("DNS field extends beyond the packet"))?;
    let value = u32::from_be_bytes(
        bytes[*cursor..end]
            .try_into()
            .map_err(|_| WireError::new("DNS field has the wrong width"))?,
    );
    *cursor = end;
    Ok(value)
}

fn take_u32_bounded(bytes: &[u8], cursor: &mut usize, bound: usize) -> Result<u32, WireError> {
    if cursor.checked_add(4).is_none_or(|end| end > bound) {
        return Err(WireError::new(
            "DNS RDATA field extends beyond its declared span",
        ));
    }
    take_u32(bytes, cursor)
}

fn put_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_be_bytes());
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::net::{Ipv4Addr, Ipv6Addr};

    pub(crate) use super::Name;
    use super::{RData as ParsedRData, WireError, parse_response, put_u16};

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) enum MessageType {
        Query,
        Response,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) enum ResponseCode {
        NoError,
        FormErr,
        ServFail,
        NXDomain,
        Refused,
    }

    #[allow(clippy::upper_case_acronyms)]
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) enum RecordType {
        A,
        AAAA,
        Other(u16),
    }

    impl RecordType {
        fn code(self) -> u16 {
            match self {
                Self::A => 1,
                Self::AAAA => 28,
                Self::Other(code) => code,
            }
        }
    }

    impl ResponseCode {
        fn code(self) -> u16 {
            match self {
                Self::NoError => 0,
                Self::FormErr => 1,
                Self::ServFail => 2,
                Self::NXDomain => 3,
                Self::Refused => 5,
            }
        }
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub(crate) struct Query {
        name: Name,
        record_type: RecordType,
    }

    impl Query {
        pub(crate) fn new(name: Name, record_type: RecordType) -> Self {
            Self { name, record_type }
        }

        pub(crate) fn name(&self) -> &Name {
            &self.name
        }

        pub(crate) fn query_type(&self) -> RecordType {
            self.record_type
        }
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) struct A(pub(crate) Ipv4Addr);

    #[allow(clippy::upper_case_acronyms)]
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) struct AAAA(pub(crate) Ipv6Addr);

    #[allow(clippy::upper_case_acronyms)]
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub(crate) struct CNAME(pub(crate) Name);

    #[allow(clippy::upper_case_acronyms)]
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub(crate) struct SOA {
        primary: Name,
        mailbox: Name,
        serial: u32,
        refresh: i32,
        retry: i32,
        expire: i32,
        minimum: u32,
    }

    impl SOA {
        #[allow(clippy::too_many_arguments)]
        pub(crate) fn new(
            primary: Name,
            mailbox: Name,
            serial: u32,
            refresh: i32,
            retry: i32,
            expire: i32,
            minimum: u32,
        ) -> Self {
            Self {
                primary,
                mailbox,
                serial,
                refresh,
                retry,
                expire,
                minimum,
            }
        }
    }

    #[allow(clippy::upper_case_acronyms)]
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub(crate) enum RData {
        A(A),
        AAAA(AAAA),
        CNAME(CNAME),
        SOA(SOA),
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub(crate) struct Record {
        name: Name,
        ttl: u32,
        data: RData,
    }

    impl Record {
        pub(crate) fn from_rdata(name: Name, ttl: u32, data: RData) -> Self {
            Self { name, ttl, data }
        }
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub(crate) struct Message {
        id: u16,
        message_type: MessageType,
        recursion_desired: bool,
        recursion_available: bool,
        truncated: bool,
        response_code: ResponseCode,
        queries: Vec<Query>,
        answers: Vec<Record>,
        name_servers: Vec<Record>,
        additionals: Vec<Record>,
    }

    impl Message {
        pub(crate) fn new() -> Self {
            Self {
                id: 0,
                message_type: MessageType::Query,
                recursion_desired: false,
                recursion_available: false,
                truncated: false,
                response_code: ResponseCode::NoError,
                queries: Vec::new(),
                answers: Vec::new(),
                name_servers: Vec::new(),
                additionals: Vec::new(),
            }
        }

        pub(crate) fn from_vec(bytes: &[u8]) -> Result<Self, WireError> {
            let parsed = parse_response(bytes)?;
            let mut message = Self::new();
            message.id = parsed.id;
            message.message_type = if parsed.is_response {
                MessageType::Response
            } else {
                MessageType::Query
            };
            message.truncated = parsed.truncated;
            message.response_code = match parsed.rcode {
                0 => ResponseCode::NoError,
                1 => ResponseCode::FormErr,
                2 => ResponseCode::ServFail,
                3 => ResponseCode::NXDomain,
                5 => ResponseCode::Refused,
                _ => return Err(WireError::new("fixture response code is unsupported")),
            };
            message.queries = parsed
                .questions
                .into_iter()
                .map(|question| {
                    let record_type = match question.record_type {
                        1 => RecordType::A,
                        28 => RecordType::AAAA,
                        other => RecordType::Other(other),
                    };
                    Query::new(question.name, record_type)
                })
                .collect();
            message.answers = parsed
                .answers
                .into_iter()
                .filter_map(convert_record)
                .collect();
            message.name_servers = parsed
                .authorities
                .into_iter()
                .filter_map(convert_record)
                .collect();
            Ok(message)
        }

        pub(crate) fn to_vec(&self) -> Result<Vec<u8>, WireError> {
            let mut output = Vec::new();
            put_u16(&mut output, self.id);
            let mut flags = 0_u16;
            if self.message_type == MessageType::Response {
                flags |= 0x8000;
            }
            if self.recursion_desired {
                flags |= 0x0100;
            }
            if self.recursion_available {
                flags |= 0x0080;
            }
            if self.truncated {
                flags |= 0x0200;
            }
            flags |= self.response_code.code();
            put_u16(&mut output, flags);
            put_count(&mut output, self.queries.len())?;
            put_count(&mut output, self.answers.len())?;
            put_count(&mut output, self.name_servers.len())?;
            put_u16(&mut output, 0);
            for query in &self.queries {
                query.name.encode(&mut output);
                put_u16(&mut output, query.record_type.code());
                put_u16(&mut output, super::CLASS_IN);
            }
            for record in &self.answers {
                encode_record(&mut output, record)?;
            }
            for record in &self.name_servers {
                encode_record(&mut output, record)?;
            }
            if output.len() > super::MAX_PACKET_LEN {
                return Err(WireError::new("fixture DNS message exceeds packet bound"));
            }
            Ok(output)
        }

        pub(crate) fn set_id(&mut self, id: u16) -> &mut Self {
            self.id = id;
            self
        }

        pub(crate) fn id(&self) -> u16 {
            self.id
        }

        pub(crate) fn set_message_type(&mut self, message_type: MessageType) -> &mut Self {
            self.message_type = message_type;
            self
        }

        pub(crate) fn set_recursion_available(&mut self, value: bool) -> &mut Self {
            self.recursion_available = value;
            self
        }

        pub(crate) fn set_truncated(&mut self, value: bool) -> &mut Self {
            self.truncated = value;
            self
        }

        pub(crate) fn set_response_code(&mut self, value: ResponseCode) -> &mut Self {
            self.response_code = value;
            self
        }

        pub(crate) fn add_query(&mut self, query: Query) -> &mut Self {
            self.queries.push(query);
            self
        }

        pub(crate) fn query(&self) -> Option<&Query> {
            self.queries.first()
        }

        pub(crate) fn queries(&self) -> &[Query] {
            &self.queries
        }

        pub(crate) fn add_answer(&mut self, record: Record) -> &mut Self {
            self.answers.push(record);
            self
        }

        pub(crate) fn answers(&self) -> &[Record] {
            &self.answers
        }

        pub(crate) fn add_name_server(&mut self, record: Record) -> &mut Self {
            self.name_servers.push(record);
            self
        }

        pub(crate) fn name_servers(&self) -> &[Record] {
            &self.name_servers
        }

        pub(crate) fn additionals(&self) -> &[Record] {
            &self.additionals
        }
    }

    fn convert_record(record: super::Record) -> Option<Record> {
        let data = match record.data {
            ParsedRData::A(address) => RData::A(A(address)),
            ParsedRData::Aaaa(address) => RData::AAAA(AAAA(address)),
            ParsedRData::Cname(name) => RData::CNAME(CNAME(name)),
            ParsedRData::Soa { minimum } => {
                RData::SOA(SOA::new(Name::root(), Name::root(), 0, 0, 0, 0, minimum))
            }
            ParsedRData::Opt { .. } | ParsedRData::Other => return None,
        };
        Some(Record::from_rdata(record.name, record.ttl, data))
    }

    fn put_count(output: &mut Vec<u8>, value: usize) -> Result<(), WireError> {
        let value = u16::try_from(value)
            .map_err(|_| WireError::new("fixture DNS section count exceeds u16"))?;
        put_u16(output, value);
        Ok(())
    }

    fn encode_record(output: &mut Vec<u8>, record: &Record) -> Result<(), WireError> {
        record.name.encode(output);
        let (record_type, rdata) = match &record.data {
            RData::A(A(address)) => (1_u16, address.octets().to_vec()),
            RData::AAAA(AAAA(address)) => (28_u16, address.octets().to_vec()),
            RData::CNAME(CNAME(name)) => {
                let mut bytes = Vec::new();
                name.encode(&mut bytes);
                (super::TYPE_CNAME, bytes)
            }
            RData::SOA(soa) => {
                let mut bytes = Vec::new();
                soa.primary.encode(&mut bytes);
                soa.mailbox.encode(&mut bytes);
                bytes.extend_from_slice(&soa.serial.to_be_bytes());
                bytes.extend_from_slice(&soa.refresh.to_be_bytes());
                bytes.extend_from_slice(&soa.retry.to_be_bytes());
                bytes.extend_from_slice(&soa.expire.to_be_bytes());
                bytes.extend_from_slice(&soa.minimum.to_be_bytes());
                (super::TYPE_SOA, bytes)
            }
        };
        put_u16(output, record_type);
        put_u16(output, super::CLASS_IN);
        output.extend_from_slice(&record.ttl.to_be_bytes());
        let length = u16::try_from(rdata.len())
            .map_err(|_| WireError::new("fixture DNS RDATA exceeds u16"))?;
        put_u16(output, length);
        output.extend_from_slice(&rdata);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_response(id: u16, name: &Name, record_type: RecordType) -> Vec<u8> {
        let mut bytes = encode_query(id, name, record_type).expect("query must encode");
        bytes[2] = 0x81;
        bytes[3] = 0x80;
        bytes
    }

    #[test]
    fn bounded_query_matches_reviewed_golden_encoding() {
        let name = Name::from_ascii("example.test").expect("NBReq name must parse");
        assert_eq!(
            encode_query(0x1234, &name, RecordType::A).expect("NBReq query must encode"),
            b"\x12\x34\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00\x07example\x04test\x00\x00\x01\x00\x01"
        );

        let punycode =
            Name::from_ascii("xn--bcher-kva.example.").expect("punycode name must parse");
        assert_eq!(
            encode_query(0xabcd, &punycode, RecordType::AAAA)
                .expect("NBReq AAAA query must encode"),
            b"\xab\xcd\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00\x0dxn--bcher-kva\x07example\x00\x00\x1c\x00\x01"
        );
    }

    #[test]
    fn parses_compressed_answer_and_case_folds_identity() {
        let expected = Name::from_ascii("Example.Test").expect("name must parse");
        let mut bytes = base_response(0x1234, &expected, RecordType::A);
        bytes[6..8].copy_from_slice(&1_u16.to_be_bytes());
        bytes.extend_from_slice(&[
            0xc0, 0x0c, // owner points at question
            0x00, 0x01, // A
            0x00, 0x01, // IN
            0x00, 0x00, 0x00, 0x1e, // TTL 30
            0x00, 0x04, 127, 0, 0, 1,
        ]);
        let parsed = parse_response(&bytes).expect("response must parse");
        assert_eq!(parsed.id, 0x1234);
        assert!(parsed.is_response);
        assert_eq!(parsed.questions[0].name, expected);
        assert_eq!(parsed.answers[0].data, RData::A(Ipv4Addr::LOCALHOST));
    }

    #[test]
    fn rejects_pointer_loops_out_of_packet_targets_and_trailing_bytes() {
        let expected = Name::from_ascii("loop.test").expect("name must parse");
        let mut looped = base_response(1, &expected, RecordType::A);
        looped[4..6].copy_from_slice(&1_u16.to_be_bytes());
        looped.truncate(DNS_HEADER_LEN);
        looped.extend_from_slice(&[0xc0, 0x0c, 0x00, 0x01, 0x00, 0x01]);
        assert_eq!(
            parse_response(&looped)
                .expect_err("loop must fail")
                .message(),
            "DNS compression pointer does not point backward"
        );

        let mut outside = base_response(2, &expected, RecordType::A);
        outside[4..6].copy_from_slice(&1_u16.to_be_bytes());
        outside.truncate(DNS_HEADER_LEN);
        outside.extend_from_slice(&[0xff, 0xff, 0x00, 0x01, 0x00, 0x01]);
        assert_eq!(
            parse_response(&outside)
                .expect_err("outside pointer must fail")
                .message(),
            "DNS compression pointer is outside the packet"
        );

        let mut trailing = base_response(3, &expected, RecordType::A);
        trailing.push(0);
        assert_eq!(
            parse_response(&trailing)
                .expect_err("trailing byte must fail")
                .message(),
            "DNS response has trailing bytes"
        );
    }

    #[test]
    fn rejects_impossible_counts_and_malformed_known_rdata() {
        let expected = Name::from_ascii("shape.test").expect("name must parse");
        let mut impossible = base_response(4, &expected, RecordType::A);
        impossible[6..8].copy_from_slice(&u16::MAX.to_be_bytes());
        assert_eq!(
            parse_response(&impossible)
                .expect_err("impossible count must fail")
                .message(),
            "DNS section counts cannot fit in the packet"
        );

        let mut bad_a = base_response(5, &expected, RecordType::A);
        bad_a[6..8].copy_from_slice(&1_u16.to_be_bytes());
        bad_a.extend_from_slice(&[
            0xc0, 0x0c, 0x00, 0x01, 0x00, 0x01, 0, 0, 0, 1, 0, 3, 127, 0, 0,
        ]);
        assert_eq!(
            parse_response(&bad_a)
                .expect_err("short A must fail")
                .message(),
            "DNS A RDATA has the wrong length"
        );
    }

    #[test]
    fn parses_soa_negative_ttl_and_extended_rcode() {
        let expected = Name::from_ascii("missing.test").expect("name must parse");
        let mut bytes = base_response(6, &expected, RecordType::A);
        bytes[8..10].copy_from_slice(&1_u16.to_be_bytes());
        bytes[10..12].copy_from_slice(&1_u16.to_be_bytes());
        bytes.extend_from_slice(&[0xc0, 0x0c]);
        put_u16(&mut bytes, TYPE_SOA);
        put_u16(&mut bytes, CLASS_IN);
        bytes.extend_from_slice(&60_u32.to_be_bytes());
        let mut soa = Vec::new();
        Name::root().encode(&mut soa);
        Name::root().encode(&mut soa);
        for value in 1_u32..=5 {
            soa.extend_from_slice(&value.to_be_bytes());
        }
        put_u16(&mut bytes, soa.len() as u16);
        bytes.extend_from_slice(&soa);
        bytes.extend_from_slice(&[
            0, // OPT owner root
            0, 41, // OPT
            0x04, 0xd0, // UDP payload size
            1, 0, 0, 0, // extended RCODE 1
            0, 0, // empty options
        ]);
        let parsed = parse_response(&bytes).expect("SOA/OPT response must parse");
        assert_eq!(parsed.rcode, 16);
        assert_eq!(parsed.authorities[0].data, RData::Soa { minimum: 5 });
    }
}
