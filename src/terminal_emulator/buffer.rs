use std::alloc::{self, Layout};
use std::ops::Range;
use thiserror::Error;

use super::TerminalData2;
use super::{recording::SnapshotItem, CursorPos};

fn align_to_size(val: usize, alignment: usize) -> usize {
    let mask = alignment - 1;
    (val + mask) & !mask
}

fn usize_aligned_offset(val: usize) -> usize {
    align_to_size(val, std::mem::align_of::<usize>())
}

fn bool_aligned_offset(val: usize) -> usize {
    align_to_size(val, std::mem::align_of::<bool>())
}

#[allow(unused)]
pub struct TerminalBufferInsertResponse {
    /// Range of written data after insertion of padding
    pub written_range: Range<usize>,
    /// Range of written data that is new. Note this will shift all data after it
    /// Includes padding that was previously not there, e.g. newlines needed to get to the
    /// requested row for writing
    pub insertion_range: Range<usize>,
    pub new_cursor_pos: CursorPos,
}

#[allow(unused)]
#[derive(Debug)]
pub struct TerminalBufferInsertLineResponse {
    /// Range of deleted data **before insertion**
    pub deleted_range: Range<usize>,
    /// Range of inserted data
    pub inserted_range: Range<usize>,
}

#[allow(unused)]
pub struct TerminalBufferSetWinSizeResponse {
    pub changed: bool,
    pub insertion_range: Range<usize>,
    pub new_cursor_pos: CursorPos,
}

mod buf_pos_keys {
    pub const LINE_ID: &str = "line_id";
    pub const X_POS: &str = "x_pos";
}
// Cursor pos <-- visible location
// FIXME: Should this be copy?
#[derive(Debug, Eq, PartialEq, PartialOrd, Ord, Clone, Copy)]
pub struct BufPos {
    /// Line id refers to a line at the time of writing in the visible buffer
    /// Once the line is in scrollback, we could have 1 "line" split over multiple visible lines,
    /// or 1 visible line containing multiple line ids
    /// This is because of resize... at any given time the visible buffer will have a one to one
    /// mapping
    /// This allows us to have a constant id for a character across visible and scrollback areas
    pub line_id: usize,
    pub x_pos: usize,
}

impl BufPos {
    pub const MAX: BufPos = BufPos {
        line_id: usize::MAX,
        x_pos: usize::MAX,
    };
    pub fn new(x_pos: usize, line_id: usize) -> BufPos {
        BufPos { x_pos, line_id }
    }

    pub fn from_snapshot(root: SnapshotItem) -> BufPos {
        use buf_pos_keys::*;
        let mut root = root.into_map().unwrap();

        let load_usize_from_i64_w_neg_1 = |v: SnapshotItem| {
            let v = v.into_i64().unwrap();
            if v == -1 {
                return usize::MAX;
            }
            v.try_into().unwrap()
        };
        let line_id = load_usize_from_i64_w_neg_1(root.remove(LINE_ID).unwrap());
        let x_pos = load_usize_from_i64_w_neg_1(root.remove(X_POS).unwrap());

        BufPos { line_id, x_pos }
    }

    pub fn snapshot(&self) -> SnapshotItem {
        use buf_pos_keys::*;
        let usize_to_i64_w_max = |v: usize| {
            if v == usize::MAX {
                return SnapshotItem::Int(-1);
            }
            v.try_into().unwrap()
        };
        SnapshotItem::Map(
            [
                (LINE_ID.to_string(), usize_to_i64_w_max(self.line_id)),
                (X_POS.to_string(), usize_to_i64_w_max(self.x_pos)),
            ]
            .into(),
        )
    }
}

/// All indexes are assumed to be y * width + x
// FIXME: Put an example
pub struct TerminalBufferModification {
    // Where in the buffer we wrote after range adjustment. What are the indexes _right now_
    pub written_range: Range<BufPos>,
    // Where is the cursor after this modification
    pub new_cursor_pos: CursorPos,
}

#[derive(Debug, Error)]
enum CreateSnapshotErrorKind {
    #[error("length offset does not fit in i64")]
    LengthNotI64(#[source] std::num::TryFromIntError),
    #[error("newline offset does not fit in i64")]
    NewlineNotI64(#[source] std::num::TryFromIntError),
    #[error("width does not fit in i64")]
    WidthNotI64(#[source] std::num::TryFromIntError),
    #[error("height does not fit in i64")]
    HeightNotI64(#[source] std::num::TryFromIntError),
    #[error("first line idx does not fit in i64")]
    FirstLineIdxNotI64(#[source] std::num::TryFromIntError),
}

#[derive(Debug, Error)]
#[error(transparent)]
pub struct CreateSnapshotError(#[from] CreateSnapshotErrorKind);

#[derive(Debug, Error)]
enum LoadSnapshotErrorKind {
    #[error("root item is not a map")]
    RootNotMap,
    #[error("visible buf item is not present")]
    VisibleBufNotPresent,
    #[error("scrollback item is not present")]
    ScrollbackNotPresent,
    #[error("scrollback item is not an array")]
    ScrollbackNotArray,
    #[error("scrollback element is not u8")]
    ScrollbackElemNotu8,
    #[error("buf item is not present")]
    BufNotPresent,
    #[error("buf item is not an array")]
    BufNotArray,
    #[error("buf element is not u8")]
    BufElemNotu8,
    #[error("{0} is not present")]
    ElemNotPresent(&'static str),
    #[error("{0} is not a usize")]
    ElemNotUsize(&'static str),
}

#[derive(Debug, Error)]
#[error(transparent)]
pub struct LoadSnapshotError(#[from] LoadSnapshotErrorKind);

struct LineInsertionResponse {
    /// How many bytes of input we ate
    consumed: usize,
    /// Where is the cursor after the insertion
    new_x_pos: usize,
}

#[derive(Debug)]
struct Line<'a> {
    buf: &'a mut [u8],
    len: &'a mut usize,
    newline: &'a mut bool,
}

impl Line<'_> {
    fn copy_from_other(&mut self, other: &Line<'_>) {
        self.buf.copy_from_slice(other.buf);
        *self.len = *other.len;
        *self.newline = *other.newline;
    }

    fn clear(&mut self) {
        *self.len = 0;
        *self.newline = false;
    }

    fn insert_spaces(&mut self, pos: usize, num_spaces: usize) {
        let num_spaces = num_spaces.min(self.buf.len() - pos);
        let dest_start = pos + num_spaces;
        let dest_end = (num_spaces + *self.len).min(self.buf.len());
        if dest_start > dest_end {
            return;
        }
        let copy_len = dest_end - dest_start;

        self.buf.copy_within(pos..pos + copy_len, dest_start);
        self.buf[pos..pos + num_spaces].fill(b' ');
        *self.len = dest_end;
    }

    fn insert_data(&mut self, data: &[u8], pos: usize) -> LineInsertionResponse {
        if pos >= self.buf.len() {
            return LineInsertionResponse {
                consumed: 0,
                new_x_pos: pos,
            };
        }

        let mut copy_len = self.buf.len() - pos;
        copy_len = copy_len.min(data.len());

        let newline_search_length = (copy_len + 1).min(data.len());
        let newline_pos = data[..newline_search_length]
            .iter()
            .position(|b| *b == b'\n');

        if let Some(pos) = newline_pos {
            copy_len = copy_len.min(pos);
            *self.newline = true;
        }

        if *self.len < pos {
            self.buf[*self.len..pos].fill(b' ');
        }

        self.buf[pos..pos + copy_len].copy_from_slice(&data[..copy_len]);
        *self.len = (*self.len).max(pos + copy_len);

        if let Some(newline_pos) = newline_pos {
            LineInsertionResponse {
                consumed: newline_pos + 1,
                new_x_pos: self.buf.len(),
            }
        } else {
            LineInsertionResponse {
                consumed: copy_len,
                new_x_pos: pos + copy_len,
            }
        }
    }

    fn serialize(&self) -> &[u8] {
        &self.buf[..*self.len]
    }
}

struct VisibleBufferSerializeResponse {
    data: Vec<u8>,
    /// Line id -> range in data
    line_mappings: Vec<usize>,
}

mod visible_buffer_keys {
    pub const BUF: &str = "buf";
    pub const LENGTH_OFFSET: &str = "length_offset";
    pub const NEWLINE_OFFSET: &str = "newline_offset";
    pub const WIDTH: &str = "width";
    pub const HEIGHT: &str = "height";
    pub const FIRST_LINE_IDX: &str = "first_line_idx";
}

#[derive(PartialEq, Debug)]
struct VisibleBuffer {
    buf: Box<[u8]>,
    length_offset: usize,
    newline_offset: usize,
    width: usize,
    height: usize,
    first_line_idx: usize,
}

impl VisibleBuffer {
    fn new(width: usize, height: usize) -> VisibleBuffer {
        let data_size = width * height;
        let length_offset = usize_aligned_offset(data_size);
        let newline_offset =
            bool_aligned_offset(length_offset + std::mem::size_of::<usize>() * height);

        let usize_alignment = std::mem::align_of::<usize>();
        let bool_alignment = std::mem::align_of::<bool>();
        let u8_alignment = std::mem::align_of::<u8>();

        assert_eq!(usize_alignment % bool_alignment, 0);
        assert_eq!(usize_alignment % u8_alignment, 0);

        let total_size = newline_offset + std::mem::size_of::<bool>() * height;

        let layout =
            Layout::from_size_align(total_size, usize_alignment).expect("invalid alloc layout");
        unsafe {
            let ptr: *mut u8 = alloc::alloc(layout);
            let slice: &mut [u8] = std::slice::from_raw_parts_mut(ptr, total_size);
            let buf: Box<[u8]> = Box::from_raw(slice as *mut [u8]);

            let mut ret = VisibleBuffer {
                buf,
                length_offset,
                newline_offset,
                width,
                height,
                first_line_idx: 0,
            };

            for y in 0..height {
                ret.get_line(y).clear();
            }
            ret
        }
    }

    fn serialize(&mut self) -> VisibleBufferSerializeResponse {
        let mut data = Vec::new();
        let width = self.width;
        let lines = self.get_all_lines();
        let mut line_mappings = Vec::new();
        let last_line_with_content = lines
            .iter()
            .enumerate()
            .rev()
            .find(|(_i, l)| *l.len > 0)
            .map(|(i, _l)| i)
            .unwrap_or(0);

        let mut line_start = 0;
        for y in 0..last_line_with_content {
            // FIXME: factor out
            let line = &lines[y];

            let next_line_is_empty_line = || lines.get(y + 1).map(|x| *x.len == 0).unwrap_or(false);

            data.extend(line.serialize());
            if *line.newline || *line.len < width || next_line_is_empty_line() {
                data.push(b'\n');
            }

            line_mappings.push(line_start);
            line_start = data.len();
        }

        data.extend(lines[last_line_with_content].serialize());
        line_mappings.push(line_start);

        for _ in last_line_with_content + 1..self.height {
            line_mappings.push(data.len());
        }

        if !data.is_empty() {
            // Last line always ends in \n
            data.push(b'\n');
        }

        VisibleBufferSerializeResponse {
            data,
            line_mappings,
        }
    }

    fn resolve_idx(&self, idx: usize) -> usize {
        (self.first_line_idx + idx) % self.height
    }

    fn get_line(&mut self, y: usize) -> Line<'_> {
        let idx = self.resolve_idx(y);
        unsafe {
            let (data, rest) = self.buf.split_at_mut(self.length_offset);
            let (lengths, newlines) = rest.split_at_mut(self.newline_offset - self.length_offset);

            let lengths_start = lengths.as_mut_ptr() as *mut usize;
            let lengths = std::slice::from_raw_parts_mut(lengths_start, self.height);

            let newlines_start = newlines.as_mut_ptr() as *mut bool;
            let newlines = std::slice::from_raw_parts_mut(newlines_start, self.height);

            Line {
                buf: &mut data[idx * self.width..idx * self.width + self.width],
                len: &mut lengths[idx],
                newline: &mut newlines[idx],
            }
        }
    }

    fn get_all_lines(&mut self) -> Vec<Line<'_>> {
        let mut ret = Vec::new();
        unsafe {
            let (mut data, rest) = self.buf.split_at_mut(self.length_offset);
            let (lengths, newlines) = rest.split_at_mut(self.newline_offset - self.length_offset);

            let lengths_start = lengths.as_mut_ptr() as *mut usize;
            let mut lengths = std::slice::from_raw_parts_mut(lengths_start, self.height);

            let newlines_start = newlines.as_mut_ptr() as *mut bool;
            let mut newlines = std::slice::from_raw_parts_mut(newlines_start, self.height);

            for _ in 0..self.height {
                let (buf, rest) = data.split_at_mut(self.width);
                data = rest;
                let (len, rest) = lengths.split_at_mut(1);
                lengths = rest;
                let (newline, rest) = newlines.split_at_mut(1);
                newlines = rest;
                ret.push(Line {
                    buf,
                    len: &mut len[0],
                    newline: &mut newline[0],
                });
            }
        }

        ret.rotate_left(self.first_line_idx);
        ret
    }

    fn push_line(&mut self) -> Line<'_> {
        self.first_line_idx = (self.first_line_idx + 1) % self.height;
        let mut line = self.get_line(self.height - 1);
        line.clear();
        line
    }

    fn from_snapshot(snapshot: SnapshotItem) -> Result<VisibleBuffer, LoadSnapshotError> {
        use visible_buffer_keys::*;
        use LoadSnapshotErrorKind::*;
        let mut root = snapshot.into_map().map_err(|_| RootNotMap)?;

        let buf = root.remove(BUF).ok_or(BufNotPresent)?;
        let buf = buf.into_vec().map_err(|_| BufNotArray)?;
        let buf: Result<Vec<_>, _> = buf.into_iter().map(|item| item.into_num::<u8>()).collect();
        let buf: Box<[u8]> = buf.map_err(|_| BufElemNotu8)?.into();

        let mut as_usize = move |key| -> Result<usize, LoadSnapshotErrorKind> {
            root.remove(key)
                .ok_or(ElemNotPresent(key))?
                .into_num::<usize>()
                .map_err(|_| ElemNotUsize(key))
        };

        let length_offset = as_usize(LENGTH_OFFSET)?;
        let newline_offset = as_usize(NEWLINE_OFFSET)?;
        let width = as_usize(WIDTH)?;
        let height = as_usize(HEIGHT)?;
        let first_line_idx = as_usize(FIRST_LINE_IDX)?;

        Ok(VisibleBuffer {
            buf,
            length_offset,
            newline_offset,
            width,
            height,
            first_line_idx,
        })
    }

    fn snapshot(&self) -> Result<SnapshotItem, CreateSnapshotError> {
        use visible_buffer_keys::*;
        use CreateSnapshotErrorKind::*;
        let length_offset: i64 = self.length_offset.try_into().map_err(LengthNotI64)?;
        let newline_offset: i64 = self.newline_offset.try_into().map_err(NewlineNotI64)?;
        let width: i64 = self.width.try_into().map_err(WidthNotI64)?;
        let height: i64 = self.height.try_into().map_err(HeightNotI64)?;
        let first_line_idx: i64 = self.first_line_idx.try_into().map_err(FirstLineIdxNotI64)?;
        Ok(SnapshotItem::Map(
            [
                (BUF.to_string(), self.buf.iter().collect()),
                (LENGTH_OFFSET.to_string(), length_offset.into()),
                (NEWLINE_OFFSET.to_string(), newline_offset.into()),
                (WIDTH.to_string(), width.into()),
                (HEIGHT.to_string(), height.into()),
                (FIRST_LINE_IDX.to_string(), first_line_idx.into()),
            ]
            .into(),
        ))
    }
}

mod terminal_buffer_keys {
    pub const VISIBLE_BUF: &str = "visible_buf";
    pub const SCROLLBACK_LINE_POS: &str = "scrollback_line_pos";
    pub const SCROLLBACK: &str = "scrollback";
}

// scrollback positions
// Vec<usize> line id -> buf pos
//
// visible positions
// current_start_id + line offset

#[derive(PartialEq, Debug)]
pub struct TerminalBuffer2 {
    visible_buf: VisibleBuffer,
    // Mapping of line id to buffer pos. E.g. line id 4 -> buf pos by scrollback_line_positions[4]
    scrollback_line_positions: Vec<usize>,
    scrollback: Vec<u8>,
}

impl TerminalBuffer2 {
    pub fn new(width: usize, height: usize) -> TerminalBuffer2 {
        let visible_buf = VisibleBuffer::new(width, height);
        TerminalBuffer2 {
            visible_buf,
            scrollback_line_positions: Vec::new(),
            scrollback: Vec::new(),
        }
    }

    pub fn from_snapshot(snapshot: SnapshotItem) -> Result<TerminalBuffer2, LoadSnapshotError> {
        use terminal_buffer_keys::*;
        use LoadSnapshotErrorKind::*;

        let mut root = snapshot.into_map().map_err(|_| RootNotMap)?;
        let visible_buf = root.remove(VISIBLE_BUF).ok_or(VisibleBufNotPresent)?;
        let visible_buf = VisibleBuffer::from_snapshot(visible_buf)?;

        let scrollback = root.remove(SCROLLBACK).ok_or(ScrollbackNotPresent)?;
        let scrollback = scrollback.into_vec().map_err(|_| ScrollbackNotArray)?;
        let scrollback: Result<Vec<_>, _> = scrollback
            .into_iter()
            .map(|item| item.into_num::<u8>())
            .collect();
        let scrollback = scrollback.map_err(|_| ScrollbackElemNotu8)?;

        let scrollback_line_positions = root.remove(SCROLLBACK_LINE_POS).unwrap();
        let scrollback_line_positions = scrollback_line_positions.into_vec().unwrap();
        let scrollback_line_positions = scrollback_line_positions
            .into_iter()
            .map(|x| x.into_num().unwrap())
            .collect();

        Ok(TerminalBuffer2 {
            scrollback,
            scrollback_line_positions,
            visible_buf,
        })
    }

    pub fn snapshot(&self) -> Result<SnapshotItem, CreateSnapshotError> {
        pub use terminal_buffer_keys::*;
        let scrollback_line_positions_i64: Vec<SnapshotItem> = self
            .scrollback_line_positions
            .iter()
            .map(|x| {
                let x: i64 = (*x).try_into().unwrap();
                SnapshotItem::Int(x)
            })
            .collect();
        let ret = SnapshotItem::Map(
            [
                (
                    SCROLLBACK.to_string(),
                    self.scrollback.clone().into_iter().collect(),
                ),
                (
                    SCROLLBACK_LINE_POS.to_string(),
                    SnapshotItem::Array(scrollback_line_positions_i64),
                ),
                (VISIBLE_BUF.to_string(), self.visible_buf.snapshot()?),
            ]
            .into(),
        );
        Ok(ret)
    }

    fn push_line_to_scrollback(&mut self) -> Line<'_> {
        let line_to_evict = self.visible_buf.get_line(0);
        self.scrollback_line_positions.push(self.scrollback.len());
        self.scrollback.extend(line_to_evict.serialize());
        if *line_to_evict.newline {
            debug!("setting newline");
            self.scrollback.push(b'\n');
        }
        self.visible_buf.push_line()
    }

    fn cursor_to_buf_pos(&self, cursor_pos: &CursorPos) -> BufPos {
        let line_id = self.scrollback_line_positions.len() + cursor_pos.y;
        let x_pos = cursor_pos.x;

        BufPos { line_id, x_pos }
    }

    pub fn insert_data(
        &mut self,
        cursor_pos: &CursorPos,
        mut data: &[u8],
    ) -> TerminalBufferModification {
        let mut x = cursor_pos.x;
        let mut y = cursor_pos.y;
        let max_y_idx = self.visible_buf.height - 1;
        debug!("{:?}", std::str::from_utf8(data));
        assert!(y <= max_y_idx);

        let write_start = self.cursor_to_buf_pos(cursor_pos);

        loop {
            if data.is_empty() {
                break;
            }

            let mut line = self.visible_buf.get_line(y);

            let response = line.insert_data(data, x);

            x = response.new_x_pos;
            if x >= self.visible_buf.width {
                x = 0;
                y += 1;
            }

            if y > max_y_idx {
                self.push_line_to_scrollback();
                y = max_y_idx;
            }
            data = &data[response.consumed..];
        }

        let new_cursor_pos = CursorPos { x, y };
        let write_end = self.cursor_to_buf_pos(&new_cursor_pos);

        TerminalBufferModification {
            written_range: write_start..write_end,
            new_cursor_pos: CursorPos { x, y },
        }
    }

    /// Inserts data, but will not wrap. If line end is hit, data stops
    pub fn insert_spaces(
        &mut self,
        cursor_pos: &CursorPos,
        num_spaces: usize,
    ) -> TerminalBufferInsertResponse {
        if cursor_pos.y >= self.visible_buf.height {
            return TerminalBufferInsertResponse {
                written_range: 0..0,
                insertion_range: 0..0,
                new_cursor_pos: cursor_pos.clone(),
            };
        }
        let mut line = self.visible_buf.get_line(cursor_pos.y);
        line.insert_spaces(cursor_pos.x, num_spaces);

        // FIXME: color tracking broken
        TerminalBufferInsertResponse {
            written_range: 0..0,
            insertion_range: 0..0,
            new_cursor_pos: cursor_pos.clone(),
        }
    }

    // Have lots of text with no newlines
    // Insert lines to pad space
    // Write long line of text again
    //
    // What happens? Does the terminal emulator behave as if there are newlines there?

    pub fn insert_lines(
        &mut self,
        cursor_pos: &CursorPos,
        num_lines: usize,
    ) -> TerminalBufferInsertLineResponse {
        let mut lines = self.visible_buf.get_all_lines();
        debug!("{:?}", lines);
        for source_idx in (cursor_pos.y..lines.len()).rev() {
            let (a, b) = lines.split_at_mut(source_idx + 1);
            let source = a
                .last_mut()
                .expect("source_idx shoul be guaranteed to be a valid element");
            if let Some(dest) = b.get_mut(num_lines - 1) {
                dest.copy_from_other(source);
            }

            source.clear();
        }

        // FIXME: Formatting completely broken
        TerminalBufferInsertLineResponse {
            deleted_range: 0..0,
            inserted_range: 0..0,
        }
    }

    pub fn clear_forwards(&mut self, cursor_pos: &CursorPos) -> Option<usize> {
        self.clear_line_forwards(cursor_pos);
        for y in cursor_pos.y + 1..self.visible_buf.height {
            let mut line = self.visible_buf.get_line(y);
            line.clear();
        }
        // FIXME: color tracking
        None
    }

    pub fn clear_line_forwards(&mut self, cursor_pos: &CursorPos) -> Option<Range<usize>> {
        let line = self.visible_buf.get_line(cursor_pos.y);
        *line.len = usize::min(cursor_pos.x, *line.len);
        // FIXME: not sure
        *line.newline = false;
        // FIXME: Color tracking is completely broken
        None
    }

    pub fn clear_all(&mut self) {
        for y in 0..self.visible_buf.height {
            let mut line = self.visible_buf.get_line(y);
            line.clear();
        }
        self.scrollback.clear();
    }

    pub fn delete_forwards(
        &mut self,
        cursor_pos: &CursorPos,
        num_chars: usize,
    ) -> Option<Range<usize>> {
        let line = self.visible_buf.get_line(cursor_pos.y);
        if cursor_pos.x > *line.len {
            return None;
        }
        let num_chars = num_chars.min(*line.len - cursor_pos.x);
        let new_end = *line.len - num_chars;
        line.buf
            .copy_within(cursor_pos.x + num_chars..*line.len, cursor_pos.x);
        *line.len = new_end;
        // FIXME: Should newline ever be cleared here?
        // FIXME: color tracking broken
        None
    }

    // FIXME: no mut
    pub fn data(&mut self) -> TerminalData2 {
        let visible_response = self.visible_buf.serialize();
        let scrollback = self.scrollback.clone();
        let scrollback_line_mappings = self.scrollback_line_positions.clone();
        //println!("scrollback: {:?}", scrollback);
        TerminalData2 {
            scrollback,
            visible: visible_response.data,
            visible_line_mappings: visible_response.line_mappings,
            scrollback_line_mappings,
        }
    }

    pub fn get_win_size(&self) -> (usize, usize) {
        (self.visible_buf.width, self.visible_buf.height)
    }

    pub fn get_visible_range(&self) -> Range<BufPos> {
        let first_visible_line_id = self.scrollback_line_positions.len();
        let end_x = self.visible_buf.width;
        let end_y = self.visible_buf.height + first_visible_line_id;

        BufPos::new(0, first_visible_line_id)..BufPos::new(end_x, end_y)
    }

    pub fn set_win_size(
        &mut self,
        width: usize,
        height: usize,
        cursor_pos: &CursorPos,
    ) -> TerminalBufferSetWinSizeResponse {
        if self.visible_buf.width == width && self.visible_buf.height == height {
            return TerminalBufferSetWinSizeResponse {
                changed: false,
                insertion_range: 0..0,
                new_cursor_pos: cursor_pos.clone(),
            };
        }

        let mut old_visible_buf =
            std::mem::replace(&mut self.visible_buf, VisibleBuffer::new(width, height));
        let old_lines = old_visible_buf.get_all_lines();

        let mut pos = CursorPos { x: 0, y: 0 };
        let mut new_cursor_pos = pos.clone();
        for (i, line) in old_lines.into_iter().enumerate() {
            // FIXME: pos, cursor_pos naming is confusing
            if i == cursor_pos.y {
                let serialized = line.serialize();
                // FIXME: out of bounds handling
                new_cursor_pos = self
                    .insert_data(&pos, &serialized[..cursor_pos.x])
                    .new_cursor_pos;
                pos = self
                    .insert_data(&pos, &serialized[cursor_pos.x..])
                    .new_cursor_pos;
            } else {
                pos = self.insert_data(&pos, line.serialize()).new_cursor_pos;
            }

            if *line.newline {
                pos = self.insert_data(&pos, b"\n").new_cursor_pos;
            }
        }

        TerminalBufferSetWinSizeResponse {
            changed: true,
            insertion_range: 0..0,
            new_cursor_pos,
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_canvas_clear_forwards() {
        let mut buffer = TerminalBuffer2::new(5, 5);
        // Push enough data to get some in scrollback
        buffer.insert_data(&CursorPos { x: 0, y: 0 }, b"012343456789\n0123456789\n1234");

        assert_eq!(
            buffer.data().visible,
            b"\
                   34567\
                   89\n\
                   01234\
                   56789\n\
                   1234\n"
        );
        buffer.clear_forwards(&CursorPos { x: 1, y: 1 });
        // Same amount of lines should be present before and after clear
        assert_eq!(
            buffer.data().visible,
            b"\
                   34567\
                   8\n"
        );

        // A few special cases.
        // 1. Truncating on beginning of line and previous char was not a newline
        let mut buffer = TerminalBuffer2::new(5, 5);
        buffer.insert_data(&CursorPos { x: 0, y: 0 }, b"012340123401234012340123401234");
        buffer.clear_forwards(&CursorPos { x: 0, y: 1 });
        assert_eq!(buffer.data().visible, b"01234\n");

        // 2. Truncating on beginning of line and previous char was a newline
        let mut buffer = TerminalBuffer2::new(5, 5);
        buffer.insert_data(
            &CursorPos { x: 0, y: 0 },
            b"01234\n0123401234012340123401234",
        );
        buffer.clear_forwards(&CursorPos { x: 0, y: 1 });
        assert_eq!(buffer.data().visible, b"01234\n");

        // 3. Truncating on a newline
        let mut buffer = TerminalBuffer2::new(5, 5);
        buffer.insert_data(&CursorPos { x: 0, y: 0 }, b"\n\n\n\n\n\n");
        buffer.clear_forwards(&CursorPos { x: 0, y: 1 });
        assert_eq!(buffer.data().visible, b"");
    }

    #[test]
    fn test_canvas_clear() {
        let mut buffer = TerminalBuffer2::new(5, 5);
        buffer.insert_data(&CursorPos { x: 0, y: 0 }, b"0123456789");
        buffer.clear_all();
        assert_eq!(buffer.data().visible, &[]);
    }

    #[test]
    fn test_terminal_buffer_overwrite_early_newline() {
        let mut buffer = TerminalBuffer2::new(5, 5);
        buffer.insert_data(&CursorPos { x: 0, y: 0 }, b"012\n3456789");
        assert_eq!(buffer.data().visible, b"012\n3456789\n");

        // Cursor pos should be calculated based off wrapping at column 5, but should not result in
        // an extra newline
        buffer.insert_data(&CursorPos { x: 2, y: 1 }, b"test");
        assert_eq!(buffer.data().visible, b"012\n34test9\n");
    }

    #[test]
    fn test_terminal_buffer_overwrite_no_newline() {
        let mut buffer = TerminalBuffer2::new(5, 5);
        buffer.insert_data(&CursorPos { x: 0, y: 0 }, b"0123456789");
        assert_eq!(buffer.data().visible, b"0123456789\n");

        // Cursor pos should be calculated based off wrapping at column 5, but should not result in
        // an extra newline
        buffer.insert_data(&CursorPos { x: 2, y: 1 }, b"test");
        assert_eq!(buffer.data().visible, b"0123456test\n");
    }

    #[test]
    fn test_terminal_buffer_overwrite_late_newline() {
        // This should behave exactly as test_terminal_buffer_overwrite_no_newline(), except with a
        // neline between lines 1 and 2
        let mut buffer = TerminalBuffer2::new(5, 5);
        buffer.insert_data(&CursorPos { x: 0, y: 0 }, b"01234\n56789");
        assert_eq!(buffer.data().visible, b"01234\n56789\n");

        buffer.insert_data(&CursorPos { x: 2, y: 1 }, b"test");
        assert_eq!(buffer.data().visible, b"01234\n56test\n");
    }

    #[test]
    fn test_terminal_buffer_insert_unallocated_data() {
        let mut buffer = TerminalBuffer2::new(10, 10);
        buffer.insert_data(&CursorPos { x: 4, y: 5 }, b"hello world");
        assert_eq!(buffer.data().visible, b"\n\n\n\n\n    hello world\n");

        buffer.insert_data(&CursorPos { x: 3, y: 2 }, b"hello world");
        assert_eq!(
            buffer.data().visible,
            b"\n\n   hello world\n\n    hello world\n"
        );
    }

    #[test]
    fn test_canvas_scrolling() {
        let mut canvas = TerminalBuffer2::new(10, 3);
        let initial_cursor_pos = CursorPos { x: 0, y: 0 };

        fn crlf(pos: &mut CursorPos, canvas: &mut TerminalBuffer2) {
            pos.x = 0;
            *pos = canvas.insert_data(pos, b"\n").new_cursor_pos;
        }

        // Simulate real terminal usage where newlines are injected with cursor moves
        let mut response = canvas.insert_data(&initial_cursor_pos, b"asdf");
        crlf(&mut response.new_cursor_pos, &mut canvas);
        let mut response = canvas.insert_data(&response.new_cursor_pos, b"xyzw");
        crlf(&mut response.new_cursor_pos, &mut canvas);
        let mut response = canvas.insert_data(&response.new_cursor_pos, b"1234");
        crlf(&mut response.new_cursor_pos, &mut canvas);
        let mut response = canvas.insert_data(&response.new_cursor_pos, b"5678");
        //crlf(&mut response.new_cursor_pos, &mut canvas);

        assert_eq!(canvas.data().scrollback, b"asdf\n");
        assert_eq!(canvas.data().visible, b"xyzw\n1234\n5678\n");
    }

    #[test]
    fn test_canvas_delete_forwards() {
        let mut canvas = TerminalBuffer2::new(10, 5);
        canvas.insert_data(&CursorPos { x: 0, y: 0 }, b"asdf\n123456789012345");

        // Test normal deletion
        let deleted_range = canvas.delete_forwards(&CursorPos { x: 1, y: 0 }, 1);

        //assert_eq!(deleted_range, Some(1..2));
        assert_eq!(canvas.data().visible, b"adf\n123456789012345\n");

        // Test deletion clamped on newline
        let deleted_range = canvas.delete_forwards(&CursorPos { x: 1, y: 0 }, 10);
        //assert_eq!(deleted_range, Some(1..3));
        assert_eq!(canvas.data().visible, b"a\n123456789012345\n");

        // Test deletion clamped on wrap
        let deleted_range = canvas.delete_forwards(&CursorPos { x: 7, y: 1 }, 10);
        //assert_eq!(deleted_range, Some(9..12));
        assert_eq!(canvas.data().visible, b"a\n1234567\n12345\n");

        // Test deletion in case where nothing is deleted
        let deleted_range = canvas.delete_forwards(&CursorPos { x: 5, y: 5 }, 10);
        //assert_eq!(deleted_range, None);
        assert_eq!(canvas.data().visible, b"a\n1234567\n12345\n");
    }

    #[test]
    fn test_canvas_insert_spaces() {
        let mut canvas = TerminalBuffer2::new(10, 5);
        canvas.insert_data(&CursorPos { x: 0, y: 0 }, b"asdf\n123456789012345");

        // Happy path
        let response = canvas.insert_spaces(&CursorPos { x: 2, y: 0 }, 2);
        //assert_eq!(response.written_range, 2..4);
        //assert_eq!(response.insertion_range, 2..4);
        assert_eq!(response.new_cursor_pos, CursorPos { x: 2, y: 0 });
        assert_eq!(canvas.data().visible, b"as  df\n123456789012345\n");

        // Truncation at newline
        let response = canvas.insert_spaces(&CursorPos { x: 2, y: 0 }, 1000);
        //assert_eq!(response.written_range, 2..10);
        //assert_eq!(response.insertion_range, 2..6);
        assert_eq!(response.new_cursor_pos, CursorPos { x: 2, y: 0 });
        assert_eq!(canvas.data().visible, b"as        \n123456789012345\n");

        // Truncation at line wrap
        let response = canvas.insert_spaces(&CursorPos { x: 4, y: 1 }, 1000);
        //assert_eq!(response.written_range, 15..21);
        //assert_eq!(
        //    response.insertion_range.start - response.insertion_range.end,
        //    0
        //);
        assert_eq!(response.new_cursor_pos, CursorPos { x: 4, y: 1 });
        assert_eq!(canvas.data().visible, b"as        \n1234      12345\n");

        // Insertion at non-existant buffer pos
        let response = canvas.insert_spaces(&CursorPos { x: 2, y: 4 }, 3);
        //assert_eq!(response.written_range, 30..33);
        //assert_eq!(response.insertion_range, 27..34);
        assert_eq!(response.new_cursor_pos, CursorPos { x: 2, y: 4 });
        assert_eq!(canvas.data().visible, b"as        \n1234      12345\n");
    }

    #[test]
    fn test_clear_line_forwards() {
        let mut canvas = TerminalBuffer2::new(10, 5);
        canvas.insert_data(&CursorPos { x: 0, y: 0 }, b"asdf\n123456789012345");

        // Nothing do delete
        let response = canvas.clear_line_forwards(&CursorPos { x: 5, y: 5 });
        assert_eq!(response, None);
        assert_eq!(canvas.data().visible, b"asdf\n123456789012345\n");

        // Hit a newline
        let response = canvas.clear_line_forwards(&CursorPos { x: 2, y: 0 });
        //assert_eq!(response, Some(2..4));
        assert_eq!(canvas.data().visible, b"as\n123456789012345\n");

        // Hit a wrap
        let response = canvas.clear_line_forwards(&CursorPos { x: 2, y: 1 });
        //assert_eq!(response, Some(5..13));
        assert_eq!(canvas.data().visible, b"as\n12\n12345\n");

        // End of screen, beginning of line, previous line has no newline
        let mut canvas = TerminalBuffer2::new(5, 5);
        // 6 lines of 012345
        canvas.insert_data(&CursorPos { x: 0, y: 0 }, b"01234012340123401234abcde0123");
        assert_eq!(canvas.data().visible, b"012340123401234abcde0123\n");
        let response = canvas.clear_line_forwards(&CursorPos { x: 0, y: 4 });
        //assert_eq!(response, Some(25..30));
        assert_eq!(canvas.data().visible, b"012340123401234abcde\n");
    }
    //
    //    #[test]
    //    fn test_resize_expand() {
    //        // Ensure that on window size increase, text stays in same spot relative to cursor position
    //        // This was problematic with our initial implementation. It's less of a problem after some
    //        // later improvements, but we can keep the test to make sure it still seems sane
    //        let mut canvas = TerminalBuffer2::new(10, 6);
    //
    //        let cursor_pos = CursorPos { x: 0, y: 0 };
    //
    //        fn simulate_resize(
    //            canvas: &mut TerminalBuffer,
    //            width: usize,
    //            height: usize,
    //            cursor_pos: &CursorPos,
    //        ) -> TerminalBufferInsertResponse {
    //            let mut response = canvas.set_win_size(width, height, cursor_pos);
    //            response.new_cursor_pos.x = 0;
    //            let mut response = canvas.insert_data(&response.new_cursor_pos, &vec![b' '; width]);
    //            response.new_cursor_pos.x = 0;
    //
    //            canvas.insert_data(&response.new_cursor_pos, b"$ ")
    //        }
    //        let response = simulate_resize(&mut canvas, 10, 5, &cursor_pos);
    //        let response = simulate_resize(&mut canvas, 10, 4, &response.new_cursor_pos);
    //        let response = simulate_resize(&mut canvas, 10, 3, &response.new_cursor_pos);
    //        simulate_resize(&mut canvas, 10, 5, &response.new_cursor_pos);
    //        assert_eq!(canvas.data().visible, b"$         \n");
    //    }
    //
    #[test]
    fn test_insert_lines() {
        let mut canvas = TerminalBuffer2::new(5, 5);

        // Test empty canvas
        let response = canvas.insert_lines(&CursorPos { x: 0, y: 0 }, 3);
        // Clear doesn't have to do anything as there's nothing in the canvas to push aside
        //assert_eq!(response.deleted_range.start - response.deleted_range.end, 0);
        //assert_eq!(
        //    response.inserted_range.start - response.inserted_range.end,
        //    0
        //);
        assert_eq!(canvas.data().visible, b"");

        // Test edge wrapped
        canvas.insert_data(&CursorPos { x: 0, y: 0 }, b"0123456789asdf\nxyzw");
        assert_eq!(canvas.data().visible, b"0123456789asdf\nxyzw\n");
        let response = canvas.insert_lines(&CursorPos { x: 3, y: 2 }, 1);
        assert_eq!(canvas.data().visible, b"0123456789\n\nasdf\nxyzw\n");
        //assert_eq!(response.deleted_range.start - response.deleted_range.end, 0);
        //assert_eq!(response.inserted_range, 10..12);

        // Test newline wrapped + lines pushed off the edge
        let response = canvas.insert_lines(&CursorPos { x: 3, y: 2 }, 1);
        assert_eq!(canvas.data().visible, b"0123456789\n\n\nasdf\n");
        //assert_eq!(response.deleted_range, 17..22);
        //assert_eq!(response.inserted_range, 11..12);
    }

    #[test]
    fn test_buffer_snapshot() {
        let mut terminal_buffer = TerminalBuffer2::new(5, 3);
        terminal_buffer.insert_data(
            &CursorPos { x: 2, y: 1 },
            b"hello world\n asdf asdf\n wrap and stuff",
        );

        let snapshot = terminal_buffer.snapshot().expect("failed to snapshot");
        let loaded = TerminalBuffer2::from_snapshot(snapshot).expect("failed to load snapshot");
        assert_eq!(terminal_buffer, loaded);
    }

    #[test]
    fn test_insertion_response() {
        let mut terminal_buffer = TerminalBuffer2::new(5, 5);
        let response = terminal_buffer.insert_data(&CursorPos { x: 0, y: 0 }, b"asdf");
        assert_eq!(response.written_range, BufPos::new(0, 0)..BufPos::new(4, 0));
        assert_eq!(response.new_cursor_pos, CursorPos { x: 4, y: 0 });

        // insertion at x 3, y 2, NOTE: no eviction
        let response = terminal_buffer.insert_data(&CursorPos { x: 3, y: 2 }, b"asdf");
        assert_eq!(response.written_range, BufPos::new(3, 2)..BufPos::new(2, 3));
        assert_eq!(response.new_cursor_pos, CursorPos { x: 2, y: 3 });
    }

    #[test]
    fn test_insertion_response_too_much_data() {
        let mut terminal_buffer = TerminalBuffer2::new(5, 5);
        let response = terminal_buffer.insert_data(
            &CursorPos { x: 0, y: 0 },
            b"0123401234012340123401234abcdeabc",
        );

        assert_eq!(response.written_range, BufPos::new(0, 0)..BufPos::new(3, 6));
        assert_eq!(response.new_cursor_pos, CursorPos { x: 3, y: 4 });

        let mut terminal_buffer = TerminalBuffer2::new(5, 5);
        let response = terminal_buffer.insert_data(
            &CursorPos { x: 0, y: 0 },
            b"01234\n01234\n01234\n01234\n01234\nabcde\nabc",
        );
        assert_eq!(response.written_range, BufPos::new(0, 0)..BufPos::new(3, 6));
        assert_eq!(response.new_cursor_pos, CursorPos { x: 3, y: 4 });
    }

    #[test]
    fn test_insertion_response_some_evicted() {
        let mut terminal_buffer = TerminalBuffer2::new(5, 5);
        let response = terminal_buffer.insert_data(&CursorPos { x: 0, y: 0 }, b"as\n");
        let response = terminal_buffer.insert_data(
            &response.new_cursor_pos,
            b"01234\n01234\n01234\n01234\n0123",
        );
        assert_eq!(
            response.written_range,
            (BufPos::new(0, 1)..BufPos::new(4, 5))
        );
        assert_eq!(response.new_cursor_pos, CursorPos { x: 4, y: 4 });
    }
}
