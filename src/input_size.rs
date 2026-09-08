//! Retained Arrow backing-buffer accounting for input byte limits.

use std::collections::HashMap;

use arrow_array::Array;

/// Estimates bytes retained by the array's visible backing buffers.
///
/// Visits all `ArrayData` children and validity buffers, deduplicating by
/// `Buffer::data_ptr()` (the backing pointer, not a slice's starting pointer).
/// Each allocation is charged the maximum of its reported capacity and every
/// visible slice's `ptr_offset() + len()`. Slices therefore retain the full known
/// capacity, and externally owned buffers with zero capacity still count their
/// visible extent. Extent calculations and the final sum saturate at `usize::MAX`.
///
/// External owners are opaque: they may retain memory beyond the exposed buffers
/// or expose one allocation through distinct backing pointers. This bounds the
/// visible buffers, not exact RSS or all memory retained by an external owner.
/// Array metadata, allocator overhead, and sharing with other inputs are excluded.
pub fn retained_buffer_bytes(array: &dyn Array) -> usize {
    let data = array.to_data();
    let mut pending = vec![&data];
    let mut allocations = HashMap::new();

    while let Some(data) = pending.pop() {
        for buffer in data
            .buffers()
            .iter()
            .chain(data.nulls().map(|nulls| nulls.buffer()))
        {
            let bytes = buffer
                .capacity()
                .max(buffer.ptr_offset().saturating_add(buffer.len()));
            allocations
                .entry(buffer.data_ptr())
                .and_modify(|retained: &mut usize| *retained = (*retained).max(bytes))
                .or_insert(bytes);
        }
        pending.extend(data.child_data());
    }

    allocations.into_values().fold(0, usize::saturating_add)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::ptr::NonNull;
    use std::sync::Arc;

    use arrow_array::{ArrayRef, BooleanArray, NullArray, RecordBatch, StructArray, UInt8Array};
    use dora_node_api::arrow_v59::buffer::{BooleanBuffer, Buffer, NullBuffer, ScalarBuffer};
    use dora_node_api::arrow_v59::ipc::{reader::StreamReader, writer::StreamWriter};
    use forge_msgs::Image;

    use super::*;

    fn image_batch(side: u32) -> RecordBatch {
        Image::new(
            side,
            side,
            "rgb8",
            side * 3,
            vec![0; side as usize * side as usize * 3].into(),
        )
        .unwrap()
        .to_record_batch()
        .unwrap()
    }

    #[test]
    fn bufferless_array_retains_no_buffer_bytes() {
        assert_eq!(retained_buffer_bytes(&NullArray::new(10)), 0);
    }

    #[test]
    fn cloned_image_columns_are_charged_once() {
        let batch = image_batch(2);
        let original = retained_buffer_bytes(&StructArray::from(batch.clone()));
        let width_bytes = retained_buffer_bytes(batch.column(1).as_ref());
        let mut columns = batch.columns().to_vec();
        columns[1] = columns[0].clone();
        let shared = RecordBatch::try_new(batch.schema(), columns).unwrap();

        assert_eq!(
            retained_buffer_bytes(&StructArray::from(shared)),
            original - width_bytes
        );
    }

    #[test]
    fn slices_retain_capacity_including_unused_space() {
        let mut values = Vec::with_capacity(4096);
        values.extend_from_slice(&[1_u8; 128]);
        let capacity = values.capacity();
        let array = UInt8Array::from(values);

        assert_eq!(retained_buffer_bytes(&array.slice(64, 1)), capacity);
        assert_eq!(retained_buffer_bytes(&array.slice(128, 0)), capacity);
    }

    #[test]
    fn nested_children_and_nulls_share_one_allocation() {
        let backing = Buffer::from_vec(vec![0b01010101_u8; 64]);
        let capacity = backing.capacity();
        let bits = BooleanBuffer::new(backing, 0, 8);
        let nulls = NullBuffer::new(bits.clone());
        let child: ArrayRef = Arc::new(BooleanArray::new(bits, Some(nulls.clone())));
        let batch =
            RecordBatch::try_from_iter([("first", child.clone()), ("second", child)]).unwrap();
        let nested: ArrayRef = Arc::new(StructArray::new(
            batch.schema().fields().clone(),
            batch.columns().to_vec(),
            Some(nulls),
        ));
        let root = StructArray::from(RecordBatch::try_from_iter([("nested", nested)]).unwrap());

        assert_eq!(retained_buffer_bytes(&root), capacity);
    }

    #[test]
    fn external_slices_count_the_largest_known_extent_in_either_order() {
        // The opaque owner retains 64 bytes, but exposes only the first 24.
        let owner = Arc::new(vec![0_u8; 64]);
        let ptr = NonNull::new(owner.as_ptr().cast_mut()).unwrap();
        // SAFETY: the immutable allocation contains at least 24 initialized bytes
        // and remains alive through the owner retained by the Buffer.
        let backing = unsafe { Buffer::from_custom_allocation(ptr, 24, owner) };
        // Arrow versions report either zero or the exposed length for custom
        // allocations. Neither reveals the owner's full retained allocation.
        let expected = backing.capacity().max(24);
        let early: ArrayRef = Arc::new(UInt8Array::new(
            ScalarBuffer::new(backing.slice_with_length(4, 4), 0, 4),
            None,
        ));
        let late: ArrayRef = Arc::new(UInt8Array::new(
            ScalarBuffer::new(backing.slice_with_length(20, 4), 0, 4),
            None,
        ));

        assert_eq!(retained_buffer_bytes(late.as_ref()), expected);
        for columns in [[early.clone(), late.clone()], [late.clone(), early.clone()]] {
            let [first, second] = columns;
            let root = StructArray::from(
                RecordBatch::try_from_iter([("first", first), ("second", second)]).unwrap(),
            );
            assert_eq!(retained_buffer_bytes(&root), expected);
        }
    }

    #[test]
    fn ipc_rgb8_image_counts_one_48_mib_body_not_seven_copies() {
        const PIXEL_BYTES: usize = 4096 * 4096 * 3;
        const MIB: usize = 1024 * 1024;
        let batch = image_batch(4096);
        let mut encoded = Vec::new();
        {
            let mut writer = StreamWriter::try_new(&mut encoded, &batch.schema()).unwrap();
            writer.write(&batch).unwrap();
            writer.finish().unwrap();
        }
        drop(batch);
        let mut reader = StreamReader::try_new(Cursor::new(encoded), None).unwrap();
        let decoded = reader.next().unwrap().unwrap();
        let array = StructArray::from(decoded);

        // IPC's three integers, string offsets/values, and binary offsets/values
        // are seven views into the same message body allocation.
        let data = array.to_data();
        let buffers: Vec<_> = data
            .child_data()
            .iter()
            .flat_map(|child| child.buffers())
            .collect();
        assert_eq!(buffers.len(), 7);
        assert!(
            buffers
                .iter()
                .all(|buffer| buffer.data_ptr() == buffers[0].data_ptr())
        );
        let retained = retained_buffer_bytes(&array);
        assert_eq!(retained, buffers[0].capacity());
        assert!((PIXEL_BYTES..PIXEL_BYTES + MIB).contains(&retained));
        assert!(array.get_buffer_memory_size() >= 7 * PIXEL_BYTES);
    }
}
