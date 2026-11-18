/*
 * SPDX-FileCopyrightText: The LineageOS Project
 * SPDX-License-Identifier: Apache-2.0
 */

//! Generic netlink messages of the kernel thermal family (linux/thermal.h). No I/O here.

const NLMSG_HDRLEN: usize = 16;
const GENL_HDRLEN: usize = 4;
const NLA_HDRLEN: usize = 4;
const NLA_TYPE_MASK: u16 = 0x3fff;
const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;
const NLM_F_REQUEST: u16 = 1;

pub const GENL_ID_CTRL: u16 = 0x10;
const CTRL_CMD_GETFAMILY: u8 = 3;
const CTRL_ATTR_FAMILY_ID: u16 = 1;
const CTRL_ATTR_FAMILY_NAME: u16 = 2;
const CTRL_ATTR_MCAST_GROUPS: u16 = 7;
const CTRL_ATTR_MCAST_GRP_NAME: u16 = 1;
const CTRL_ATTR_MCAST_GRP_ID: u16 = 2;

pub const THERMAL_FAMILY: &str = "thermal";
pub const SAMPLING_GROUP: &str = "sampling";
pub const EVENT_GROUP: &str = "event";

const ATTR_TZ_ID: u16 = 2;
const ATTR_TZ_TEMP: u16 = 3;
const ATTR_TZ_TRIP_ID: u16 = 5;
const ATTR_TZ_NAME: u16 = 10;
const ATTR_CDEV_ID: u16 = 15;
const ATTR_CDEV_CUR_STATE: u16 = 16;

// The sampling group carries cmd SAMPLING_TEMP (0), the event group the event commands.
const SAMPLING_TEMP: u8 = 0;
const EVENT_TZ_CREATE: u8 = 1;
const EVENT_TZ_TRIP_UP: u8 = 5;
const EVENT_TZ_TRIP_DOWN: u8 = 6;
const EVENT_CDEV_ADD: u8 = 10;
const EVENT_CDEV_STATE_UPDATE: u8 = 12;

fn align(len: usize) -> usize {
    (len + 3) & !3
}

fn u16_at(buf: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_ne_bytes(buf.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(buf: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(buf.get(at..at + 4)?.try_into().ok()?))
}

/// The netlink attributes in `buf` as (type, payload).
fn attributes(mut buf: &[u8]) -> Vec<(u16, &[u8])> {
    let mut attrs = Vec::new();
    while buf.len() >= NLA_HDRLEN {
        let len = u16_at(buf, 0).unwrap() as usize;
        let kind = u16_at(buf, 2).unwrap() & NLA_TYPE_MASK;
        if len < NLA_HDRLEN || len > buf.len() {
            break;
        }
        attrs.push((kind, &buf[NLA_HDRLEN..len]));
        buf = &buf[align(len).min(buf.len())..];
    }
    attrs
}

fn attr<'a>(attrs: &[(u16, &'a [u8])], kind: u16) -> Option<&'a [u8]> {
    attrs.iter().find(|(k, _)| *k == kind).map(|(_, v)| *v)
}

fn attr_u32(attrs: &[(u16, &[u8])], kind: u16) -> Option<u32> {
    u32_at(attr(attrs, kind)?, 0)
}

fn attr_string(attrs: &[(u16, &[u8])], kind: u16) -> Option<String> {
    let bytes = attr(attrs, kind)?;
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    Some(String::from_utf8_lossy(&bytes[..end]).into_owned())
}

/// One netlink message: (type, generic netlink command, attributes).
fn messages(buf: &[u8]) -> Vec<(u16, u8, Vec<(u16, &[u8])>)> {
    let mut out = Vec::new();
    let mut rest = buf;
    while rest.len() >= NLMSG_HDRLEN {
        let len = u32_at(rest, 0).unwrap() as usize;
        let kind = u16_at(rest, 4).unwrap();
        if len < NLMSG_HDRLEN || len > rest.len() {
            break;
        }
        if kind != NLMSG_ERROR && kind != NLMSG_DONE && len >= NLMSG_HDRLEN + GENL_HDRLEN {
            let cmd = rest[NLMSG_HDRLEN];
            out.push((kind, cmd, attributes(&rest[NLMSG_HDRLEN + GENL_HDRLEN..len])));
        }
        rest = &rest[align(len).min(rest.len())..];
    }
    out
}

/// A CTRL_CMD_GETFAMILY request for `family`.
pub fn get_family_request(family: &str, seq: u32) -> Vec<u8> {
    let name_len = family.len() + 1;
    let attr_len = NLA_HDRLEN + name_len;
    let len = NLMSG_HDRLEN + GENL_HDRLEN + align(attr_len);
    let mut msg = Vec::with_capacity(len);
    msg.extend_from_slice(&(len as u32).to_ne_bytes());
    msg.extend_from_slice(&GENL_ID_CTRL.to_ne_bytes());
    msg.extend_from_slice(&NLM_F_REQUEST.to_ne_bytes());
    msg.extend_from_slice(&seq.to_ne_bytes());
    msg.extend_from_slice(&0u32.to_ne_bytes());
    msg.extend_from_slice(&[CTRL_CMD_GETFAMILY, 1, 0, 0]);
    msg.extend_from_slice(&(attr_len as u16).to_ne_bytes());
    msg.extend_from_slice(&CTRL_ATTR_FAMILY_NAME.to_ne_bytes());
    msg.extend_from_slice(family.as_bytes());
    msg.resize(len, 0);
    msg
}

/// The family id and multicast groups (name, id) from a CTRL_CMD_NEWFAMILY reply.
pub fn parse_family(buf: &[u8]) -> Option<(u16, Vec<(String, u32)>)> {
    let (_, _, attrs) = messages(buf).into_iter().find(|(kind, _, _)| *kind == GENL_ID_CTRL)?;
    let id = u16_at(attr(&attrs, CTRL_ATTR_FAMILY_ID)?, 0)?;
    let groups = attr(&attrs, CTRL_ATTR_MCAST_GROUPS)
        .map(|nested| {
            attributes(nested)
                .into_iter()
                .filter_map(|(_, group)| {
                    let group = attributes(group);
                    Some((
                        attr_string(&group, CTRL_ATTR_MCAST_GRP_NAME)?,
                        attr_u32(&group, CTRL_ATTR_MCAST_GRP_ID)?,
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    Some((id, groups))
}

#[derive(Debug, PartialEq)]
pub enum Event {
    TripCrossed { zone: i32, trip: i32 },
    Sample { zone: i32, temperature: i64 },
    ZoneCreated { zone: i32, name: String },
    CdevAdded { cdev: i32 },
    CdevState { cdev: i32, state: i64 },
}

/// The thermal events in a datagram from the thermal family `family`.
pub fn parse_events(buf: &[u8], family: u16) -> Vec<Event> {
    messages(buf)
        .into_iter()
        .filter(|(kind, _, _)| *kind == family)
        .filter_map(|(_, cmd, attrs)| {
            let u32_or = |kind, default: i64| attr_u32(&attrs, kind).map_or(default, i64::from);
            // Temperatures are signed in a u32 attribute.
            let signed = |kind| attr_u32(&attrs, kind).map(|v| v as i32);
            match cmd {
                // The sampling group's only command shares its value with EVENT_UNSPEC.
                SAMPLING_TEMP => Some(Event::Sample {
                    zone: signed(ATTR_TZ_ID)?,
                    temperature: i64::from(signed(ATTR_TZ_TEMP)?),
                }),
                EVENT_TZ_TRIP_UP | EVENT_TZ_TRIP_DOWN => Some(Event::TripCrossed {
                    zone: signed(ATTR_TZ_ID)?,
                    trip: signed(ATTR_TZ_TRIP_ID).unwrap_or(-1),
                }),
                EVENT_TZ_CREATE => Some(Event::ZoneCreated {
                    zone: signed(ATTR_TZ_ID)?,
                    name: attr_string(&attrs, ATTR_TZ_NAME).unwrap_or_default(),
                }),
                EVENT_CDEV_ADD => Some(Event::CdevAdded { cdev: signed(ATTR_CDEV_ID)? }),
                EVENT_CDEV_STATE_UPDATE => Some(Event::CdevState {
                    cdev: signed(ATTR_CDEV_ID)?,
                    state: u32_or(ATTR_CDEV_CUR_STATE, -1),
                }),
                _ => None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nla(kind: u16, payload: &[u8]) -> Vec<u8> {
        let mut a = Vec::new();
        a.extend_from_slice(&((NLA_HDRLEN + payload.len()) as u16).to_ne_bytes());
        a.extend_from_slice(&kind.to_ne_bytes());
        a.extend_from_slice(payload);
        a.resize(align(a.len()), 0);
        a
    }

    fn nlmsg(kind: u16, cmd: u8, attrs: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = attrs.concat();
        let len = NLMSG_HDRLEN + GENL_HDRLEN + body.len();
        let mut m = Vec::new();
        m.extend_from_slice(&(len as u32).to_ne_bytes());
        m.extend_from_slice(&kind.to_ne_bytes());
        m.extend_from_slice(&[0; 10]);
        m.extend_from_slice(&[cmd, 2, 0, 0]);
        m.extend_from_slice(&body);
        m
    }

    #[test]
    fn request_layout() {
        let msg = get_family_request("thermal", 7);
        assert_eq!(msg.len(), 16 + 4 + 12);
        assert_eq!(u32_at(&msg, 0), Some(32));
        assert_eq!(u16_at(&msg, 4), Some(GENL_ID_CTRL));
        assert_eq!(msg[16], CTRL_CMD_GETFAMILY);
        assert_eq!(u16_at(&msg, 20), Some(12));
        assert_eq!(&msg[24..32], b"thermal\0");
    }

    #[test]
    fn family_reply() {
        let group = |name: &str, id: u32| {
            let mut n = name.as_bytes().to_vec();
            n.push(0);
            nla(1, &[nla(CTRL_ATTR_MCAST_GRP_NAME, &n), nla(CTRL_ATTR_MCAST_GRP_ID, &id.to_ne_bytes())].concat())
        };
        let reply = nlmsg(
            GENL_ID_CTRL,
            1,
            &[
                nla(CTRL_ATTR_FAMILY_ID, &30u16.to_ne_bytes()),
                nla(CTRL_ATTR_FAMILY_NAME, b"thermal\0"),
                nla(CTRL_ATTR_MCAST_GROUPS, &[group("sampling", 8), group("event", 9)].concat()),
            ],
        );
        let (id, groups) = parse_family(&reply).unwrap();
        assert_eq!(id, 30);
        assert_eq!(groups, vec![("sampling".to_owned(), 8), ("event".to_owned(), 9)]);
    }

    #[test]
    fn events_in_one_datagram() {
        let neg = (-5000i32) as u32;
        let datagram = [
            nlmsg(30, SAMPLING_TEMP, &[nla(ATTR_TZ_ID, &4u32.to_ne_bytes()), nla(ATTR_TZ_TEMP, &neg.to_ne_bytes())]),
            nlmsg(30, EVENT_TZ_TRIP_UP, &[nla(ATTR_TZ_ID, &7u32.to_ne_bytes()), nla(ATTR_TZ_TRIP_ID, &1u32.to_ne_bytes())]),
            nlmsg(31, EVENT_TZ_TRIP_UP, &[nla(ATTR_TZ_ID, &8u32.to_ne_bytes())]),
            nlmsg(30, EVENT_TZ_CREATE, &[nla(ATTR_TZ_ID, &9u32.to_ne_bytes()), nla(ATTR_TZ_NAME, b"skin\0")]),
            nlmsg(30, EVENT_CDEV_STATE_UPDATE, &[nla(ATTR_CDEV_ID, &3u32.to_ne_bytes()), nla(ATTR_CDEV_CUR_STATE, &2u32.to_ne_bytes())]),
            nlmsg(30, EVENT_CDEV_ADD, &[nla(ATTR_CDEV_ID, &42u32.to_ne_bytes()), nla(18, b"modem_lte_dsc\0")]),
        ]
        .concat();
        assert_eq!(
            parse_events(&datagram, 30),
            vec![
                Event::Sample { zone: 4, temperature: -5000 },
                Event::TripCrossed { zone: 7, trip: 1 },
                Event::ZoneCreated { zone: 9, name: "skin".to_owned() },
                Event::CdevState { cdev: 3, state: 2 },
                Event::CdevAdded { cdev: 42 },
            ]
        );
    }

    #[test]
    fn truncated_input_is_ignored() {
        let msg = nlmsg(30, EVENT_TZ_TRIP_UP, &[nla(ATTR_TZ_ID, &7u32.to_ne_bytes())]);
        assert!(parse_events(&msg[..msg.len() - 3], 30).is_empty());
        assert!(parse_events(&[1, 2, 3], 30).is_empty());
    }
}
