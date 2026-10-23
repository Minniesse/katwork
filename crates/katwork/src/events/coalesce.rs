use std::time::Duration;
use tokio::time::Instant;
use unicode_segmentation::UnicodeSegmentation;

pub struct DeltaCoalescer {
    buffer: String,
    released_at: Instant,
}

impl Default for DeltaCoalescer {
    fn default() -> Self {
        Self::new()
    }
}

impl DeltaCoalescer {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            released_at: Instant::now(),
        }
    }

    pub fn push(&mut self, delta: &str, now: Instant) -> Option<String> {
        self.buffer.push_str(delta);
        if self.buffer.len() < 512
            && now.duration_since(self.released_at) < Duration::from_millis(100)
        {
            return None;
        }
        // The final cluster can still grow when the next provider chunk arrives.
        let (mut cut, _) = self.buffer.grapheme_indices(true).next_back()?;
        while self.buffer[..cut].ends_with(['เ', 'แ', 'โ', 'ใ', 'ไ']) {
            cut -= 3;
        }
        if cut == 0 {
            return None;
        }
        let tail = self.buffer.split_off(cut);
        self.released_at = now;
        Some(std::mem::replace(&mut self.buffer, tail))
    }

    pub fn finish(&mut self) -> Option<String> {
        self.released_at = Instant::now();
        (!self.buffer.is_empty()).then(|| std::mem::take(&mut self.buffer))
    }
}
