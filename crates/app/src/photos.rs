//! Contacts' pictures, decoded for the avatars on screen as they are asked
//! for.

use std::cell::RefCell;
use std::collections::HashMap;

use lookout_core::Store;
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

// Pictures kept decoded; past this many they are all dropped and decoded
// again as needed. Each is up to 81 KB (144 pixels square), so at most
// some 6 MB.
const KEPT: usize = 80;

#[derive(Default)]
pub struct Photos {
    /// By contact, as `account/id`; `None` when there is no picture.
    decoded: RefCell<HashMap<String, Option<Image>>>,
    /// The contact of each address, lowercased, read once: most people in
    /// a message list aren't contacts, and are then found at no cost.
    contacts: RefCell<Option<HashMap<String, (String, String)>>>,
}

impl Photos {
    /// The picture of the contact with this address; empty when none.
    pub fn for_address(&self, store: &Store, address: &str) -> Image {
        let contact = {
            let mut contacts = self.contacts.borrow_mut();
            let contacts = contacts.get_or_insert_with(|| {
                let addresses = store.contact_addresses().unwrap_or_default();
                addresses.into_iter().map(|(address, account, id)| (address, (account, id))).collect()
            });
            contacts.get(&address.trim().to_lowercase()).cloned()
        };
        match contact {
            Some((account, id)) => self.for_contact(store, &account, &id),
            None => Image::default(),
        }
    }

    pub fn for_contact(&self, store: &Store, account: &str, id: &str) -> Image {
        self.get(format!("{account}/{id}"), || store.contact_photo(account, id).ok().flatten())
    }

    /// Contacts changed or new pictures were found: both are read again.
    pub fn clear(&self) {
        self.decoded.borrow_mut().clear();
        self.contacts.borrow_mut().take();
    }

    fn get(&self, key: String, read: impl FnOnce() -> Option<Vec<u8>>) -> Image {
        if let Some(found) = self.decoded.borrow().get(&key) {
            return found.clone().unwrap_or_default();
        }
        let image = read().and_then(|bytes| decode(&bytes));
        let mut decoded = self.decoded.borrow_mut();
        if decoded.len() >= KEPT {
            decoded.clear();
        }
        decoded.insert(key, image.clone());
        image.unwrap_or_default()
    }
}

fn decode(bytes: &[u8]) -> Option<Image> {
    let picture = image::load_from_memory(bytes).ok()?.into_rgba8();
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(picture.as_raw(), picture.width(), picture.height());
    round(&mut buffer);
    Some(Image::from_rgba8(buffer))
}

/// Makes a square picture round: the software renderer doesn't clip to
/// rounded corners. The edge is softened over a pixel.
pub fn round(buffer: &mut SharedPixelBuffer<Rgba8Pixel>) {
    let (width, height) = (buffer.width() as usize, buffer.height() as usize);
    let radius = width.min(height) as f32 / 2.0;
    let (cx, cy) = (width as f32 / 2.0, height as f32 / 2.0);
    for (index, pixel) in buffer.make_mut_slice().iter_mut().enumerate() {
        let (x, y) = ((index % width) as f32 + 0.5, (index / width) as f32 + 0.5);
        let outside = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() - radius + 0.5;
        if outside > 0.0 {
            // Premultiplied alpha is not used: only alpha changes.
            pixel.a = (pixel.a as f32 * (1.0 - outside.min(1.0))) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_are_made_round() {
        let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(32, 32);
        buffer.make_mut_slice().fill(Rgba8Pixel { r: 10, g: 20, b: 30, a: 255 });
        round(&mut buffer);
        let pixels = buffer.as_slice();
        assert_eq!(pixels[0].a, 0);
        assert_eq!(pixels[16 * 32 + 16].a, 255);
        // The middle of an edge is all but whole.
        assert!(pixels[16 * 32].a > 240);
        assert_eq!(pixels[31 * 32 + 31].a, 0);
    }
}
