# yeet wire protocol (YET1)

One TCP connection per transfer attempt. All integers are little-endian.

## Framing

The sender first writes the 4-byte magic `YET1`. After that, both directions are a stream of frames:

```
[kind u8][len u32][payload: len bytes]
```

A frame payload is at most 256 MiB (64 × `BLOCK`). An unknown `kind` or codec is a protocol error and ends the connection.

`str16` below means `[len u16][UTF-8 bytes]`. Strings longer than 65535 bytes are cut on a character boundary.

## Frame kinds

| kind | name | direction | payload |
|---:|---|---|---|
| 1 | DIR | sender → receiver | `path` (UTF-8, rest of payload) |
| 2 | FILE | sender → receiver | `id u64, size u64, path` (UTF-8, rest of payload) |
| 3 | CHUNK | sender → receiver | see below |
| 4 | DONE | sender → receiver | `files u64, bytes u64` (totals of the whole transfer) |
| 5 | SUMMARY | receiver → sender | `files u64, bytes u64` (what was written) |
| 6 | OFFER | sender → receiver | `transfer u64, files u64, bytes u64, name str16, count u16, items str16 × count` |
| 7 | ACCEPT | receiver → sender | `media u8, fs u8`: the destination drive (media 0 unknown, 1 HDD, 2 SSD, 3 network; fs 0 other, 1 NTFS, 2 ReFS, 3 exFAT, 4 FAT). Unknown values mean unknown. |
| 8 | DECLINE | receiver → sender | empty |
| 9 | HAVE | receiver → sender | `count u32, (id u64, written u64) × count` |
| 10 | ACK | receiver → sender | `files u64, bytes u64` (progress so far) |
| 11 | CANCEL | either | empty |

Paths are relative to the destination and use `/` separators. The receiver rejects absolute paths, `..`, and anything else that would escape the destination folder.

OFFER's `name` is the sending device's name. `items` holds the top-level file and folder names, which are shown to the person.

### CHUNK

```
codec u8          0 = none, 1 = zstd
raw_len u32       decoded size, equal to the sum of segment lengths
count u16
segments          (id u64, offset u64, len u32) × count
data              the rest of the payload
```

Decoded `data` is the segments' bytes concatenated in order. Every segment says which file and offset it belongs to, so chunks can be written in any order.

The sender cuts large files into chunks of up to 4 MiB (`BLOCK`). It bundles files smaller than 1 MiB (`SMALL`) many to a chunk.

## Conversation

```
sender                                     receiver
  "YET1" OFFER                         ──▶
                                       ◀──  ACCEPT HAVE        (or DECLINE)
  DIR... FILE/CHUNK... DONE            ──▶
                                       ◀──  ACK every 300 ms
                                       ◀──  SUMMARY
```

1. The receiver asks the person about the OFFER and answers ACCEPT or DECLINE. The sender waits up to 120 s for the answer.
2. After ACCEPT, the receiver sends HAVE. On a new transfer HAVE is empty.
3. The sender sends every DIR first, then one FILE per file. Ids are 0, 1, 2… in a stable scan order. CHUNKs for a file may follow as soon as its FILE has been sent. Empty files have a FILE frame and no chunks.
4. DONE ends the stream. The receiver finishes writing, then replies with SUMMARY.

ACK frames carry progress and also act as a heartbeat. If the sender hears nothing for 10 s, it treats the connection as lost.

## Cancel and resume

**Cancel.** Either side may send CANCEL at any time to stop for good. The receiver then deletes partly written files and keeps completed ones.

Sometimes the sender can't deliver CANCEL on the current connection: the connection is dead or stuck, or the sender is between reconnects. In that case it opens a fresh connection and sends `"YET1" OFFER CANCEL` for the same transfer, then reads until the receiver closes. The receiver treats this like any resume, so the CANCEL ends the transfer instead of leaving it waiting for a resume.

**Resume.** A dropped connection does not end the transfer. The sender reconnects for up to 5 minutes and sends the same OFFER again, with the same `transfer` id. It looks the device up again each time, so a new IP address is fine.

If the receiver still has that transfer, it reattaches without asking the person again. The receiver keeps a transfer for 10 minutes. Its HAVE lists every file it knows about and how many bytes are already written. The sender then:

- skips the files that are complete,
- resumes partial files from the written offset, without resending their FILE frame,
- sends everything else as normal.

## Versioning

Any incompatible change bumps the magic (`YET1` → `YET2`). Peers with different magics refuse each other, and the app reports this as "running a different version of yeet".

## Discovery

Devices announce themselves over mDNS as `_yeet._tcp.local.`. Each announcement carries a stable device id and a display name, and the service port is the TCP port to connect to. Discovery only finds devices; it isn't required. A device can also be reached directly at `host:port`, and the default port is 7878.
