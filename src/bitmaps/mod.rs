//! Bitmap API

// Main docs: https://hwloc.readthedocs.io/en/v2.9/group__hwlocality__bitmap.html

mod indices;

#[cfg(doc)]
use crate::{
    cpu::cpusets::CpuSet,
    memory::nodesets::NodeSet,
    objects::TopologyObject,
    topology::{builder::BuildFlags, Topology},
};
use crate::{
    errors,
    ffi::{self, IncompleteType},
    Sealed,
};
#[cfg(any(test, feature = "quickcheck"))]
use quickcheck::{Arbitrary, Gen};
use std::{
    borrow::Borrow,
    clone::Clone,
    cmp::Ordering,
    convert::TryFrom,
    ffi::{c_int, c_uint},
    fmt::{self, Debug, Display},
    iter::{FromIterator, FusedIterator},
    marker::PhantomData,
    ops::{
        BitAnd, BitAndAssign, BitOr, BitOrAssign, BitXor, BitXorAssign, Bound, Deref, Not,
        RangeBounds, Sub, SubAssign,
    },
    ptr::NonNull,
};

// Re-export BitmapIndex, the fact that it's in a separate module is an
// implementation detail / valiant attempt to fight source file growth
pub use indices::BitmapIndex;

/// Opaque bitmap struct
///
/// Represents the private `hwloc_bitmap_s` type that `hwloc_bitmap_t` API
/// pointers map to.
#[doc(alias = "hwloc_bitmap_s")]
#[doc(hidden)]
#[repr(C)]
pub struct RawBitmap(IncompleteType);

/// A generic bitmap, understood by hwloc
///
/// The `Bitmap` type represents a set of integers (positive or null). A bitmap
/// may be of infinite size (all bits are set after some point). A bitmap may
/// even be full if all bits are set.
///
/// Bitmaps are used by hwloc to represent sets of OS processors (which may
/// actually be hardware threads), via the [`CpuSet`] newtype, or to represent
/// sets of NUMA memory nodes via the [`NodeSet`] newtype.
///
/// Both [`CpuSet`] and [`NodeSet`] are always indexed by OS physical number.
/// However, users should usually not build CPU and node sets manually (e.g.
/// with [`Bitmap::set()`]). One should rather use the cpu/node sets of existing
/// [`TopologyObject`]s and combine them through boolean operations. For
/// instance, binding the current thread on a pair of cores may be performed
/// with:
///
/// ```
/// # use anyhow::Context;
/// # use hwlocality::{
/// #     cpu::{binding::CpuBindingFlags, cpusets::CpuSet},
/// #     objects::{types::ObjectType},
/// #     topology::support::{CpuBindingSupport, FeatureSupport},
/// # };
/// #
/// # let topology = hwlocality::Topology::test_instance();
/// #
/// // We want Cores, but we tolerate PUs on platforms that don't expose Cores
/// // (either there are no hardware threads or hwloc could not detect them)
/// let core_depth = topology.depth_or_below_for_type(ObjectType::Core)?;
///
/// // Yields the first two cores of a multicore system, or
/// // the only core of a single-core system
/// let cores = topology.objects_at_depth(core_depth).take(2);
///
/// // Compute the union of these cores' CPUsets, that's our CPU binding bitmap
/// let set = cores.fold(
///     CpuSet::new(),
///     |acc, core| { acc | core.cpuset().expect("Cores should have CPUsets") }
/// );
///
/// // Only actually bind if the platform supports it (macOS notably doesn't)
/// if topology.supports(FeatureSupport::cpu_binding, CpuBindingSupport::set_thread) {
///     topology.bind_cpu(&set, CpuBindingFlags::THREAD)?;
/// }
/// #
/// # Ok::<(), anyhow::Error>(())
/// ```
///
/// # Panics
///
/// Unlike most hwloc entry points in this crate, `Bitmap` functions always
/// handle unexpected hwloc errors by panicking. The rationale for this is that
/// the bitmap is just a simple data structures, without any kind of
/// complicated interactions with the operating system, for which the only
/// failure mode should be running out of memory. And panicking is the normal
/// way to handle this in Rust.
///
/// [`CpuSet`]: crate::cpu::cpusets::CpuSet
/// [`NodeSet`]: crate::memory::nodesets::NodeSet
#[doc(alias = "hwloc_bitmap_t")]
#[doc(alias = "hwloc_const_bitmap_t")]
#[repr(transparent)]
pub struct Bitmap(NonNull<RawBitmap>);

// NOTE: Remember to keep the method signatures and first doc lines in
//       impl_newtype_ops in sync with what's going on below.
impl Bitmap {
    // === FFI interoperability ===

    /// Wraps an owned hwloc_bitmap_t
    ///
    /// # Safety
    ///
    /// If non-null, the pointer must target a valid bitmap that we will acquire
    /// ownership of and automatically free on Drop.
    pub(crate) unsafe fn from_owned_raw_mut(bitmap: *mut RawBitmap) -> Option<Self> {
        NonNull::new(bitmap).map(|ptr| unsafe { Self::from_owned_nonnull(ptr) })
    }

    /// Wraps an owned hwloc bitmap
    ///
    /// # Safety
    ///
    /// The pointer must target a valid bitmap that we will acquire ownership of
    /// and automatically free on Drop.
    pub(crate) unsafe fn from_owned_nonnull(bitmap: NonNull<RawBitmap>) -> Self {
        Self(bitmap)
    }

    /// Wraps a borrowed hwloc_const_bitmap_t
    ///
    /// # Safety
    ///
    /// If non-null, the pointer must target a bitmap that is valid for 'target.
    /// Unlike with from_raw, it will not be automatically freed on Drop.
    pub(crate) unsafe fn borrow_from_raw<'target>(
        bitmap: *const RawBitmap,
    ) -> Option<BitmapRef<'target, Self>> {
        unsafe { Self::borrow_from_raw_mut(bitmap.cast_mut()) }
    }

    /// Wraps a borrowed hwloc_bitmap_t
    ///
    /// # Safety
    ///
    /// If non-null, the pointer must target a bitmap that is valid for 'target.
    /// Unlike with from_raw, it will not be automatically freed on Drop.
    pub(crate) unsafe fn borrow_from_raw_mut<'target>(
        bitmap: *mut RawBitmap,
    ) -> Option<BitmapRef<'target, Self>> {
        NonNull::new(bitmap).map(|ptr| unsafe { Self::borrow_from_nonnull(ptr) })
    }

    /// Wraps a borrowed hwloc bitmap
    ///
    /// # Safety
    ///
    /// The pointer must target a bitmap that is valid for 'target.
    /// Unlike with from_raw, it will not be automatically freed on Drop.
    pub(crate) unsafe fn borrow_from_nonnull<'target>(
        bitmap: NonNull<RawBitmap>,
    ) -> BitmapRef<'target, Self> {
        BitmapRef(bitmap, PhantomData)
    }

    // NOTE: There is no borrow_mut_from_raw because it would not be safe as if
    //       you expose an &mut Bitmap, the user can trigger Drop.

    /// Contained bitmap pointer (for interaction with hwloc)
    pub(crate) fn as_ptr(&self) -> *const RawBitmap {
        self.0.as_ptr()
    }

    /// Contained mutable bitmap pointer (for interaction with hwloc)
    pub(crate) fn as_mut_ptr(&mut self) -> *mut RawBitmap {
        self.0.as_ptr()
    }

    // === Constructors ===

    /// Creates an empty `Bitmap`
    ///
    /// # Examples
    ///
    /// ```
    /// use hwlocality::bitmaps::Bitmap;
    ///
    /// let empty = Bitmap::new();
    /// assert!(empty.is_empty());
    /// ```
    #[doc(alias = "hwloc_bitmap_alloc")]
    pub fn new() -> Self {
        unsafe {
            let ptr =
                errors::call_hwloc_ptr_mut("hwloc_bitmap_alloc", || ffi::hwloc_bitmap_alloc())
                    .expect("Bitmap operation failures are handled via panics");
            Self::from_owned_nonnull(ptr)
        }
    }

    /// Creates a full `Bitmap`
    ///
    /// # Examples
    ///
    /// ```
    /// use hwlocality::bitmaps::Bitmap;
    ///
    /// let full = Bitmap::full();
    /// assert!(full.is_full());
    /// ```
    #[doc(alias = "hwloc_bitmap_alloc_full")]
    pub fn full() -> Self {
        unsafe {
            let ptr = errors::call_hwloc_ptr_mut("hwloc_bitmap_alloc_full", || {
                ffi::hwloc_bitmap_alloc_full()
            })
            .expect("Bitmap operation failures are handled via panics");
            Self::from_owned_nonnull(ptr)
        }
    }

    /// Creates a new `Bitmap` with the given range of indices set
    ///
    /// # Examples
    ///
    /// ```
    /// use hwlocality::bitmaps::Bitmap;
    ///
    /// let bitmap = Bitmap::from_range(12..=34);
    /// assert_eq!(format!("{bitmap}"), "12-34");
    /// ```
    ///
    /// # Panics
    ///
    /// If `range` goes beyond the implementation-defined maximum index (at
    /// least 2^15-1, usually 2^31-1).
    pub fn from_range<Idx>(range: impl RangeBounds<Idx>) -> Self
    where
        Idx: Copy + PartialEq + TryInto<BitmapIndex>,
        <Idx as TryInto<BitmapIndex>>::Error: Debug,
    {
        let mut bitmap = Self::new();
        bitmap.set_range(range);
        bitmap
    }

    // === Getters and setters ===

    /// Turn this `Bitmap` into a copy of another `Bitmap`
    ///
    /// # Examples
    ///
    /// ```
    /// use hwlocality::bitmaps::Bitmap;
    ///
    /// let bitmap = Bitmap::from_range(12..=34);
    /// let mut bitmap2 = Bitmap::new();
    /// bitmap2.copy_from(&bitmap);
    /// assert_eq!(format!("{bitmap2}"), "12-34");
    /// ```
    #[doc(alias = "hwloc_bitmap_copy")]
    pub fn copy_from(&mut self, other: &Self) {
        errors::call_hwloc_int_normal("hwloc_bitmap_copy", || unsafe {
            ffi::hwloc_bitmap_copy(self.as_mut_ptr(), other.as_ptr())
        })
        .expect("Bitmap operation failures are handled via panics");
    }

    /// Clear all indices
    ///
    /// # Examples
    ///
    /// ```
    /// use hwlocality::bitmaps::Bitmap;
    ///
    /// let mut bitmap = Bitmap::from_range(12..=34);
    /// bitmap.clear();
    /// assert!(bitmap.is_empty());
    /// ```
    #[doc(alias = "hwloc_bitmap_zero")]
    pub fn clear(&mut self) {
        unsafe { ffi::hwloc_bitmap_zero(self.as_mut_ptr()) }
    }

    /// Set all indices
    ///
    /// # Examples
    ///
    /// ```
    /// use hwlocality::bitmaps::Bitmap;
    ///
    /// let mut bitmap = Bitmap::from_range(12..=34);
    /// bitmap.fill();
    /// assert!(bitmap.is_full());
    /// ```
    #[doc(alias = "hwloc_bitmap_fill")]
    pub fn fill(&mut self) {
        unsafe { ffi::hwloc_bitmap_fill(self.as_mut_ptr()) }
    }

    /// Clear all indices except for `idx`, which is set
    ///
    /// # Examples
    ///
    /// ```
    /// use hwlocality::bitmaps::Bitmap;
    ///
    /// let mut bitmap = Bitmap::from_range(12..=34);
    /// bitmap.set_only(42);
    /// assert_eq!(format!("{bitmap}"), "42");
    /// ```
    ///
    /// # Panics
    ///
    /// If `idx` is above the implementation-defined maximum index (at least
    /// 2^15-1, usually 2^31-1).
    #[doc(alias = "hwloc_bitmap_only")]
    pub fn set_only<Idx>(&mut self, idx: Idx)
    where
        Idx: TryInto<BitmapIndex>,
        <Idx as TryInto<BitmapIndex>>::Error: Debug,
    {
        let idx = idx.try_into().expect("Unsupported bitmap index");
        errors::call_hwloc_int_normal("hwloc_bitmap_only", || unsafe {
            ffi::hwloc_bitmap_only(self.as_mut_ptr(), idx.into_c_uint())
        })
        .expect("Bitmap operation failures are handled via panics");
    }

    /// Set all indices except for `idx`, which is cleared
    ///
    /// # Examples
    ///
    /// ```
    /// use hwlocality::bitmaps::Bitmap;
    ///
    /// let mut bitmap = Bitmap::from_range(12..=34);
    /// bitmap.set_all_but(42);
    /// assert_eq!(format!("{bitmap}"), "0-41,43-");
    /// ```
    ///
    /// # Panics
    ///
    /// If `idx` is above the implementation-defined maximum index (at least
    /// 2^15-1, usually 2^31-1).
    #[doc(alias = "hwloc_bitmap_allbut")]
    pub fn set_all_but<Idx>(&mut self, idx: Idx)
    where
        Idx: TryInto<BitmapIndex>,
        <Idx as TryInto<BitmapIndex>>::Error: Debug,
    {
        let idx = idx.try_into().expect("Unsupported bitmap index");
        errors::call_hwloc_int_normal("hwloc_bitmap_allbut", || unsafe {
            ffi::hwloc_bitmap_allbut(self.as_mut_ptr(), idx.into_c_uint())
        })
        .expect("Bitmap operation failures are handled via panics");
    }

    /// Set index `idx`
    ///
    /// # Examples
    ///
    /// ```
    /// use hwlocality::bitmaps::Bitmap;
    ///
    /// let mut bitmap = Bitmap::from_range(12..=34);
    /// bitmap.set(42);
    /// assert_eq!(format!("{bitmap}"), "12-34,42");
    /// ```
    ///
    /// # Panics
    ///
    /// If `idx` is above the implementation-defined maximum index (at least
    /// 2^15-1, usually 2^31-1).
    #[doc(alias = "hwloc_bitmap_set")]
    pub fn set<Idx>(&mut self, idx: Idx)
    where
        Idx: TryInto<BitmapIndex>,
        <Idx as TryInto<BitmapIndex>>::Error: Debug,
    {
        let idx = idx.try_into().expect("Unsupported bitmap index");
        errors::call_hwloc_int_normal("hwloc_bitmap_set", || unsafe {
            ffi::hwloc_bitmap_set(self.as_mut_ptr(), idx.into_c_uint())
        })
        .expect("Bitmap operation failures are handled via panics");
    }

    /// Set indices covered by `range`
    ///
    /// # Examples
    ///
    /// ```
    /// use hwlocality::bitmaps::Bitmap;
    ///
    /// let mut bitmap = Bitmap::from_range(12..=56);
    /// bitmap.set_range(34..=78);
    /// assert_eq!(format!("{bitmap}"), "12-78");
    ///
    /// bitmap.set_range(2..);
    /// assert_eq!(format!("{bitmap}"), "2-");
    /// ```
    ///
    /// # Panics
    ///
    /// If `range` goes beyond the implementation-defined maximum index (at
    /// least 2^15-1, usually 2^31-1).
    #[doc(alias = "hwloc_bitmap_set_range")]
    pub fn set_range<Idx>(&mut self, range: impl RangeBounds<Idx>)
    where
        Idx: Copy + PartialEq + TryInto<BitmapIndex>,
        <Idx as TryInto<BitmapIndex>>::Error: Debug,
    {
        if (range.start_bound(), range.end_bound()) == (Bound::Unbounded, Bound::Unbounded) {
            self.fill();
            return;
        }

        let (begin, end) = Self::hwloc_range(range);
        errors::call_hwloc_int_normal("hwloc_bitmap_set_range", || unsafe {
            ffi::hwloc_bitmap_set_range(self.as_mut_ptr(), begin, end)
        })
        .expect("Bitmap operation failures are handled via panics");
    }

    /// Clear index `idx`
    ///
    /// # Examples
    ///
    /// ```
    /// use hwlocality::bitmaps::Bitmap;
    ///
    /// let mut bitmap = Bitmap::from_range(12..=34);
    /// bitmap.unset(24);
    /// assert_eq!(format!("{bitmap}"), "12-23,25-34");
    /// ```
    ///
    /// # Panics
    ///
    /// If `idx` is above the implementation-defined maximum index (at least
    /// 2^15-1, usually 2^31-1).
    #[doc(alias = "hwloc_bitmap_clr")]
    pub fn unset<Idx>(&mut self, idx: Idx)
    where
        Idx: TryInto<BitmapIndex>,
        <Idx as TryInto<BitmapIndex>>::Error: Debug,
    {
        let idx = idx.try_into().expect("Unsupported bitmap index");
        errors::call_hwloc_int_normal("hwloc_bitmap_clr", || unsafe {
            ffi::hwloc_bitmap_clr(self.as_mut_ptr(), idx.into_c_uint())
        })
        .expect("Bitmap operation failures are handled via panics");
    }

    /// Clear indices covered by `range`
    ///
    /// # Examples
    ///
    /// ```
    /// use hwlocality::bitmaps::Bitmap;
    ///
    /// let mut bitmap = Bitmap::from_range(12..=34);
    /// bitmap.unset_range(14..=18);
    /// assert_eq!(format!("{bitmap}"), "12-13,19-34");
    ///
    /// bitmap.unset_range(26..);
    /// assert_eq!(format!("{bitmap}"), "12-13,19-25");
    /// ```
    ///
    /// # Panics
    ///
    /// If `range` goes beyond the implementation-defined maximum index (at
    /// least 2^15-1, usually 2^31-1).
    #[doc(alias = "hwloc_bitmap_clr_range")]
    pub fn unset_range<Idx>(&mut self, range: impl RangeBounds<Idx>)
    where
        Idx: Copy + PartialEq + TryInto<BitmapIndex>,
        <Idx as TryInto<BitmapIndex>>::Error: Debug,
    {
        if (range.start_bound(), range.end_bound()) == (Bound::Unbounded, Bound::Unbounded) {
            self.clear();
            return;
        }

        let (begin, end) = Self::hwloc_range(range);
        errors::call_hwloc_int_normal("hwloc_bitmap_clr_range", || unsafe {
            ffi::hwloc_bitmap_clr_range(self.as_mut_ptr(), begin, end)
        })
        .expect("Bitmap operation failures are handled via panics");
    }

    /// Keep a single index among those set in the bitmap
    ///
    /// May be useful before binding so that the process does not have a
    /// chance of migrating between multiple logical CPUs in the original mask.
    /// Instead of running the task on any PU inside the given CPU set, the
    /// operating system scheduler will be forced to run it on a single of these
    /// PUs. It avoids a migration overhead and cache-line ping-pongs between PUs.
    ///
    /// This function is NOT meant to distribute multiple processes within a
    /// single CPU set. It always return the same single bit when called
    /// multiple times on the same input set. [`Topology::distribute_items()`]
    /// may be used for generating CPU sets to distribute multiple tasks below a
    /// single multi-PU object.
    ///
    /// The effect of singlifying an empty bitmap is not specified by hwloc.
    ///
    /// # Examples
    ///
    /// ```
    /// use hwlocality::bitmaps::Bitmap;
    ///
    /// let mut bitmap = Bitmap::from_range(12..=34);
    /// bitmap.singlify();
    /// assert_eq!(bitmap.weight(), Some(1));
    /// ```
    #[doc(alias = "hwloc_bitmap_singlify")]
    pub fn singlify(&mut self) {
        errors::call_hwloc_int_normal("hwloc_bitmap_singlify", || unsafe {
            ffi::hwloc_bitmap_singlify(self.as_mut_ptr())
        })
        .expect("Bitmap operation failures are handled via panics");
    }

    /// Check if index `idx` is set
    ///
    /// # Examples
    ///
    /// ```
    /// use hwlocality::bitmaps::Bitmap;
    ///
    /// let mut bitmap = Bitmap::from_range(12..=34);
    /// assert!((0..12).all(|idx| !bitmap.is_set(idx)));
    /// assert!((12..=34).all(|idx| bitmap.is_set(idx)));
    /// assert!(!bitmap.is_set(35));
    /// ```
    ///
    /// # Panics
    ///
    /// If `idx` is above the implementation-defined maximum index (at least
    /// 2^15-1, usually 2^31-1).
    #[doc(alias = "hwloc_bitmap_isset")]
    pub fn is_set<Idx>(&self, idx: Idx) -> bool
    where
        Idx: TryInto<BitmapIndex>,
        <Idx as TryInto<BitmapIndex>>::Error: Debug,
    {
        let idx = idx.try_into().expect("Unsupported bitmap index");
        errors::call_hwloc_bool("hwloc_bitmap_isset", || unsafe {
            ffi::hwloc_bitmap_isset(self.as_ptr(), idx.into_c_uint())
        })
        .expect("Should not involve faillible syscalls")
    }

    /// Check if all indices are unset
    ///
    /// # Examples
    ///
    /// ```
    /// use hwlocality::bitmaps::Bitmap;
    ///
    /// assert!(Bitmap::new().is_empty());
    /// assert!(!Bitmap::from_range(12..=34).is_empty());
    /// assert!(!Bitmap::full().is_empty());
    /// ```
    #[doc(alias = "hwloc_bitmap_iszero")]
    pub fn is_empty(&self) -> bool {
        errors::call_hwloc_bool("hwloc_bitmap_iszero", || unsafe {
            ffi::hwloc_bitmap_iszero(self.as_ptr())
        })
        .expect("Should not involve faillible syscalls")
    }

    /// Check if all indices are set
    ///
    /// # Examples
    ///
    /// ```
    /// use hwlocality::bitmaps::Bitmap;
    ///
    /// assert!(!Bitmap::new().is_full());
    /// assert!(!Bitmap::from_range(12..=34).is_full());
    /// assert!(Bitmap::full().is_full());
    /// ```
    #[doc(alias = "hwloc_bitmap_isfull")]
    pub fn is_full(&self) -> bool {
        errors::call_hwloc_bool("hwloc_bitmap_isfull", || unsafe {
            ffi::hwloc_bitmap_isfull(self.as_ptr())
        })
        .expect("Should not involve faillible syscalls")
    }

    /// Check the first set index, if any
    ///
    /// You can iterate over set indices with [`Bitmap::iter_set()`].
    ///
    /// # Examples
    ///
    /// ```
    /// use hwlocality::bitmaps::Bitmap;
    ///
    /// let first_set_usize = |b: Bitmap| b.first_set().map(usize::from);
    /// assert_eq!(Bitmap::new().first_set(), None);
    /// assert_eq!(first_set_usize(Bitmap::from_range(12..=34)), Some(12));
    /// assert_eq!(first_set_usize(Bitmap::full()), Some(0));
    /// ```
    #[doc(alias = "hwloc_bitmap_first")]
    pub fn first_set(&self) -> Option<BitmapIndex> {
        let result = unsafe { ffi::hwloc_bitmap_first(self.as_ptr()) };
        assert!(
            result >= -1,
            "hwloc_bitmap_first returned error code {result}"
        );
        BitmapIndex::try_from_c_int(result).ok()
    }

    /// Iterate over set indices
    ///
    /// # Examples
    ///
    /// ```
    /// use hwlocality::bitmaps::Bitmap;
    ///
    /// let bitmap = Bitmap::from_range(12..=21);
    /// let indices = bitmap.iter_set().map(usize::from).collect::<Vec<_>>();
    /// assert_eq!(indices, &[12, 13, 14, 15, 16, 17, 18, 19, 20, 21]);
    /// ```
    #[doc(alias = "hwloc_bitmap_foreach_begin")]
    #[doc(alias = "hwloc_bitmap_foreach_end")]
    #[doc(alias = "hwloc_bitmap_next")]
    pub fn iter_set(&self) -> BitmapIterator<&Bitmap> {
        BitmapIterator::new(self, Bitmap::next_set)
    }

    /// Check the last set index, if any
    ///
    /// # Examples
    ///
    /// ```
    /// use hwlocality::bitmaps::Bitmap;
    ///
    /// let last_set_usize = |b: Bitmap| b.last_set().map(usize::from);
    /// assert_eq!(Bitmap::new().last_set(), None);
    /// assert_eq!(last_set_usize(Bitmap::from_range(12..=34)), Some(34));
    /// assert_eq!(Bitmap::full().last_set(), None);
    /// ```
    #[doc(alias = "hwloc_bitmap_last")]
    pub fn last_set(&self) -> Option<BitmapIndex> {
        let result = unsafe { ffi::hwloc_bitmap_last(self.as_ptr()) };
        assert!(
            result >= -1,
            "hwloc_bitmap_last returned error code {result}"
        );
        BitmapIndex::try_from_c_int(result).ok()
    }

    /// The number of indices that are set in the bitmap.
    ///
    /// None means that an infinite number of indices are set.
    ///
    /// # Examples
    ///
    /// ```
    /// use hwlocality::bitmaps::Bitmap;
    ///
    /// assert_eq!(Bitmap::new().weight(), Some(0));
    /// assert_eq!(Bitmap::from_range(12..34).weight(), Some(34-12));
    /// assert_eq!(Bitmap::full().weight(), None);
    /// ```
    #[doc(alias = "hwloc_bitmap_weight")]
    pub fn weight(&self) -> Option<usize> {
        let result = unsafe { ffi::hwloc_bitmap_weight(self.as_ptr()) };
        assert!(
            result >= -1,
            "hwloc_bitmap_weight returned error code {result}"
        );
        usize::try_from(result).ok()
    }

    /// Check the first unset index, if any
    ///
    /// You can iterate over set indices with [`Bitmap::iter_unset()`].
    ///
    /// # Examples
    ///
    /// ```
    /// use hwlocality::bitmaps::Bitmap;
    ///
    /// let first_unset_usize = |b: Bitmap| b.first_unset().map(usize::from);
    /// assert_eq!(first_unset_usize(Bitmap::new()), Some(0));
    /// assert_eq!(first_unset_usize(Bitmap::from_range(..12)), Some(12));
    /// assert_eq!(Bitmap::full().first_unset(), None);
    /// ```
    #[doc(alias = "hwloc_bitmap_first_unset")]
    pub fn first_unset(&self) -> Option<BitmapIndex> {
        let result = unsafe { ffi::hwloc_bitmap_first_unset(self.as_ptr()) };
        assert!(
            result >= -1,
            "hwloc_bitmap_first_unset returned error code {result}"
        );
        BitmapIndex::try_from_c_int(result).ok()
    }

    /// Iterate over unset indices
    ///
    /// # Examples
    ///
    /// ```
    /// use hwlocality::bitmaps::Bitmap;
    ///
    /// let bitmap = Bitmap::from_range(12..);
    /// let indices = bitmap.iter_unset().map(usize::from).collect::<Vec<_>>();
    /// assert_eq!(indices, &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
    /// ```
    #[doc(alias = "hwloc_bitmap_next_unset")]
    pub fn iter_unset(&self) -> BitmapIterator<&Bitmap> {
        BitmapIterator::new(self, Bitmap::next_unset)
    }

    /// Check the last unset index, if any
    ///
    /// # Examples
    ///
    /// ```
    /// use hwlocality::bitmaps::Bitmap;
    ///
    /// let last_unset_usize = |b: Bitmap| b.last_unset().map(usize::from);
    /// assert_eq!(Bitmap::new().last_unset(), None);
    /// assert_eq!(last_unset_usize(Bitmap::from_range(12..)), Some(11));
    /// assert_eq!(Bitmap::full().last_unset(), None);
    /// ```
    #[doc(alias = "hwloc_bitmap_last_unset")]
    pub fn last_unset(&self) -> Option<BitmapIndex> {
        let result = unsafe { ffi::hwloc_bitmap_last_unset(self.as_ptr()) };
        assert!(
            result >= -1,
            "hwloc_bitmap_last_unset returned error code {result}"
        );
        BitmapIndex::try_from_c_int(result).ok()
    }

    /// Inverts the current `Bitmap`.
    ///
    /// # Examples
    ///
    /// ```
    /// use hwlocality::bitmaps::Bitmap;
    ///
    /// let mut bitmap = Bitmap::from_range(12..=34);
    /// bitmap.invert();
    /// assert_eq!(format!("{bitmap}"), "0-11,35-");
    /// ```
    pub fn invert(&mut self) {
        errors::call_hwloc_int_normal("hwloc_bitmap_not", || unsafe {
            ffi::hwloc_bitmap_not(self.as_mut_ptr(), self.as_ptr())
        })
        .expect("Bitmap operation failures are handled via panics");
    }

    /// Truth that `self` and `rhs` have some set indices in common
    ///
    /// # Examples
    ///
    /// ```
    /// use hwlocality::bitmaps::Bitmap;
    ///
    /// let bitmap1 = Bitmap::from_range(12..=34);
    /// let bitmap2 = Bitmap::from_range(56..=78);
    /// assert!(!bitmap1.intersects(&bitmap2));
    ///
    /// let bitmap3 = Bitmap::from_range(34..=56);
    /// assert!(bitmap1.intersects(&bitmap3));
    /// assert!(bitmap2.intersects(&bitmap3));
    /// ```
    #[doc(alias = "hwloc_bitmap_intersects")]
    pub fn intersects(&self, rhs: &Self) -> bool {
        errors::call_hwloc_bool("hwloc_bitmap_intersects", || unsafe {
            ffi::hwloc_bitmap_intersects(self.as_ptr(), rhs.as_ptr())
        })
        .expect("Should not involve faillible syscalls")
    }

    /// Truth that the indices set in `inner` are a subset of those set in `self`.
    ///
    /// The empty bitmap is considered included in any other bitmap.
    ///
    /// # Examples
    ///
    /// ```
    /// use hwlocality::bitmaps::Bitmap;
    ///
    /// let bitmap1 = Bitmap::from_range(12..=78);
    /// let bitmap2 = Bitmap::from_range(34..=56);
    /// assert!(bitmap1.includes(&bitmap2));
    /// assert!(!bitmap2.includes(&bitmap1));
    /// ```
    #[doc(alias = "hwloc_bitmap_isincluded")]
    pub fn includes(&self, inner: &Self) -> bool {
        errors::call_hwloc_bool("hwloc_bitmap_isincluded", || unsafe {
            ffi::hwloc_bitmap_isincluded(inner.as_ptr(), self.as_ptr())
        })
        .expect("Should not involve faillible syscalls")
    }

    // NOTE: When adding new methods, remember to add them to impl_newtype_ops too

    // === Implementation details ===

    /// Convert a Rust range to an hwloc range
    ///
    /// # Panics
    ///
    /// If `range` goes beyond the implementation-defined maximum index (at
    /// least 2^15-1, usually 2^31-1).
    fn hwloc_range<Idx>(range: impl RangeBounds<Idx>) -> (c_uint, c_int)
    where
        Idx: Copy + TryInto<BitmapIndex>,
        <Idx as TryInto<BitmapIndex>>::Error: Debug,
    {
        // Helper that literally translates the Rust range to an hwloc range if
        // possible (shifting indices forwards/backwards to account for
        // exclusive bounds). Panics if the user-specified bounds are too high,
        // return None if they're fine but a literal translation cannot be done.
        let helper = || -> Option<(c_uint, c_int)> {
            let convert_idx = |idx: Idx| idx.try_into().ok();
            let start_idx = |idx| convert_idx(idx).expect("Range start is too high for hwloc");
            let start = match range.start_bound() {
                Bound::Unbounded => BitmapIndex::MIN,
                Bound::Included(i) => start_idx(*i),
                Bound::Excluded(i) => start_idx(*i).checked_succ()?,
            };
            let end_idx = |idx| convert_idx(idx).expect("Range end is too high for hwloc");
            let end = match range.end_bound() {
                Bound::Unbounded => -1,
                Bound::Included(i) => end_idx(*i).into_c_int(),
                Bound::Excluded(i) => end_idx(*i).checked_pred()?.into_c_int(),
            };
            Some((start.into_c_uint(), end))
        };

        // If a literal translation is not possible, it means either the start
        // bound is BitmapIndex::MAX exclusive or the end bound is
        // BitmapIndex::MIN exclusive. In both cases, the range covers no
        // indices and can be replaced by any other empty range, including 1..=0
        helper().unwrap_or((1, 0))
    }

    /// Iterator building block
    fn next(
        &self,
        index: Option<BitmapIndex>,
        next_fn: impl FnOnce(*const RawBitmap, c_int) -> c_int,
    ) -> Option<BitmapIndex> {
        let result = next_fn(
            self.as_ptr(),
            index.map(BitmapIndex::into_c_int).unwrap_or(-1),
        );
        assert!(
            result >= -1,
            "hwloc bitmap iterator returned error code {result}"
        );
        BitmapIndex::try_from_c_int(result).ok()
    }

    /// Set index iterator building block
    fn next_set(&self, index: Option<BitmapIndex>) -> Option<BitmapIndex> {
        self.next(index, |bitmap, prev| unsafe {
            ffi::hwloc_bitmap_next(bitmap, prev)
        })
    }

    /// Unset index iterator building block
    fn next_unset(&self, index: Option<BitmapIndex>) -> Option<BitmapIndex> {
        self.next(index, |bitmap, prev| unsafe {
            ffi::hwloc_bitmap_next_unset(bitmap, prev)
        })
    }
}

#[cfg(any(test, feature = "quickcheck"))]
impl Arbitrary for Bitmap {
    fn arbitrary(g: &mut Gen) -> Self {
        use std::collections::HashSet;

        // Start with an arbitrary finite bitmap
        let mut result = HashSet::<BitmapIndex>::arbitrary(g)
            .into_iter()
            .collect::<Bitmap>();

        // Decide by coin flip to extend infinitely on the right or not
        if bool::arbitrary(g) {
            let last = result.last_set().unwrap_or(BitmapIndex::MIN);
            result.set_range(last..);
        }

        result
    }

    fn shrink(&self) -> Box<dyn Iterator<Item = Self>> {
        // If this is infinite, start by removing the infinite part
        let mut local = self.clone();
        if local.weight().is_none() {
            local.unset_range(self.last_unset().unwrap_or(BitmapIndex::MIN)..);
        }

        // Now this is finite, can convert to Vec<BitmapIndex> and use Vec's shrinker
        let vec = local.into_iter().collect::<Vec<_>>();
        Box::new(vec.shrink().map(|vec| vec.into_iter().collect::<Bitmap>()))
    }
}

impl<B: Borrow<Bitmap>> BitAnd<B> for &Bitmap {
    type Output = Bitmap;

    #[doc(alias = "hwloc_bitmap_and")]
    fn bitand(self, rhs: B) -> Bitmap {
        let mut result = Bitmap::new();
        errors::call_hwloc_int_normal("hwloc_bitmap_and", || unsafe {
            ffi::hwloc_bitmap_and(result.as_mut_ptr(), self.as_ptr(), rhs.borrow().as_ptr())
        })
        .expect("Bitmap operation failures are handled via panics");
        result
    }
}

impl<B: Borrow<Bitmap>> BitAnd<B> for Bitmap {
    type Output = Bitmap;

    fn bitand(mut self, rhs: B) -> Bitmap {
        self &= rhs.borrow();
        self
    }
}

impl<B: Borrow<Bitmap>> BitAndAssign<B> for Bitmap {
    fn bitand_assign(&mut self, rhs: B) {
        errors::call_hwloc_int_normal("hwloc_bitmap_and", || unsafe {
            ffi::hwloc_bitmap_and(self.as_mut_ptr(), self.as_ptr(), rhs.borrow().as_ptr())
        })
        .expect("Bitmap operation failures are handled via panics");
    }
}

impl<B: Borrow<Bitmap>> BitOr<B> for &Bitmap {
    type Output = Bitmap;

    #[doc(alias = "hwloc_bitmap_or")]
    fn bitor(self, rhs: B) -> Bitmap {
        let mut result = Bitmap::new();
        errors::call_hwloc_int_normal("hwloc_bitmap_or", || unsafe {
            ffi::hwloc_bitmap_or(result.as_mut_ptr(), self.as_ptr(), rhs.borrow().as_ptr())
        })
        .expect("Bitmap operation failures are handled via panics");
        result
    }
}

impl<B: Borrow<Bitmap>> BitOr<B> for Bitmap {
    type Output = Bitmap;

    fn bitor(mut self, rhs: B) -> Bitmap {
        self |= rhs.borrow();
        self
    }
}

impl<B: Borrow<Bitmap>> BitOrAssign<B> for Bitmap {
    fn bitor_assign(&mut self, rhs: B) {
        errors::call_hwloc_int_normal("hwloc_bitmap_or", || unsafe {
            ffi::hwloc_bitmap_or(self.as_mut_ptr(), self.as_ptr(), rhs.borrow().as_ptr())
        })
        .expect("Bitmap operation failures are handled via panics");
    }
}

impl<B: Borrow<Bitmap>> BitXor<B> for &Bitmap {
    type Output = Bitmap;

    #[doc(alias = "hwloc_bitmap_xor")]
    fn bitxor(self, rhs: B) -> Bitmap {
        let mut result = Bitmap::new();
        errors::call_hwloc_int_normal("hwloc_bitmap_xor", || unsafe {
            ffi::hwloc_bitmap_xor(result.as_mut_ptr(), self.as_ptr(), rhs.borrow().as_ptr())
        })
        .expect("Bitmap operation failures are handled via panics");
        result
    }
}

impl<B: Borrow<Bitmap>> BitXor<B> for Bitmap {
    type Output = Bitmap;

    fn bitxor(mut self, rhs: B) -> Bitmap {
        self ^= rhs.borrow();
        self
    }
}

impl<B: Borrow<Bitmap>> BitXorAssign<B> for Bitmap {
    fn bitxor_assign(&mut self, rhs: B) {
        errors::call_hwloc_int_normal("hwloc_bitmap_xor", || unsafe {
            ffi::hwloc_bitmap_xor(self.as_mut_ptr(), self.as_ptr(), rhs.borrow().as_ptr())
        })
        .expect("Bitmap operation failures are handled via panics");
    }
}

impl Clone for Bitmap {
    #[doc(alias = "hwloc_bitmap_dup")]
    fn clone(&self) -> Bitmap {
        unsafe {
            let ptr = errors::call_hwloc_ptr_mut("hwloc_bitmap_dup", || {
                ffi::hwloc_bitmap_dup(self.as_ptr())
            })
            .expect("Bitmap operation failures are handled via panics");
            Self::from_owned_nonnull(ptr)
        }
    }
}

impl Debug for Bitmap {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        <Self as Display>::fmt(self, f)
    }
}

impl Default for Bitmap {
    fn default() -> Self {
        Self::new()
    }
}

impl Display for Bitmap {
    #[doc(alias = "hwloc_bitmap_list_snprintf")]
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        ffi::write_snprintf(f, |buf, len| unsafe {
            ffi::hwloc_bitmap_list_snprintf(buf, len, self.as_ptr())
        })
    }
}

impl Drop for Bitmap {
    #[doc(alias = "hwloc_bitmap_free")]
    fn drop(&mut self) {
        unsafe { ffi::hwloc_bitmap_free(self.as_mut_ptr()) }
    }
}

impl Eq for Bitmap {}

impl<BI: Borrow<BitmapIndex>> Extend<BI> for Bitmap {
    fn extend<T: IntoIterator<Item = BI>>(&mut self, iter: T) {
        for i in iter {
            self.set(*i.borrow());
        }
    }
}

impl<BI: Borrow<BitmapIndex>> From<BI> for Bitmap {
    fn from(value: BI) -> Self {
        Self::from_iter(std::iter::once(value))
    }
}

impl<BI: Borrow<BitmapIndex>> FromIterator<BI> for Bitmap {
    fn from_iter<I: IntoIterator<Item = BI>>(iter: I) -> Self {
        let mut bitmap = Self::new();
        bitmap.extend(iter);
        bitmap
    }
}

/// Iterator over set or unset [`Bitmap`] indices
#[derive(Copy, Clone)]
pub struct BitmapIterator<B> {
    /// Bitmap over which we're iterating
    bitmap: B,

    /// Last explored index
    prev: Option<BitmapIndex>,

    /// Mapping from last index to next index
    next: fn(&Bitmap, Option<BitmapIndex>) -> Option<BitmapIndex>,
}
//
impl<B> BitmapIterator<B> {
    fn new(bitmap: B, next: fn(&Bitmap, Option<BitmapIndex>) -> Option<BitmapIndex>) -> Self {
        Self {
            bitmap,
            prev: None,
            next,
        }
    }
}
//
impl<B: Borrow<Bitmap>> Iterator for BitmapIterator<B> {
    type Item = BitmapIndex;

    fn next(&mut self) -> Option<BitmapIndex> {
        self.prev = (self.next)(self.bitmap.borrow(), self.prev);
        self.prev
    }
}
//
impl<B: Borrow<Bitmap>> FusedIterator for BitmapIterator<B> {}
//
impl<'bitmap> IntoIterator for &'bitmap Bitmap {
    type Item = BitmapIndex;
    type IntoIter = BitmapIterator<&'bitmap Bitmap>;

    fn into_iter(self) -> Self::IntoIter {
        BitmapIterator::new(self, Bitmap::next_set)
    }
}
//
impl IntoIterator for Bitmap {
    type Item = BitmapIndex;
    type IntoIter = BitmapIterator<Bitmap>;

    fn into_iter(self) -> Self::IntoIter {
        BitmapIterator::new(self, Bitmap::next_set)
    }
}

impl Not for &Bitmap {
    type Output = Bitmap;

    #[doc(alias = "hwloc_bitmap_not")]
    fn not(self) -> Bitmap {
        let mut result = Bitmap::new();
        errors::call_hwloc_int_normal("hwloc_bitmap_not", || unsafe {
            ffi::hwloc_bitmap_not(result.as_mut_ptr(), self.as_ptr())
        })
        .expect("Bitmap operation failures are handled via panics");
        result
    }
}

impl Not for Bitmap {
    type Output = Bitmap;

    fn not(mut self) -> Self {
        self.invert();
        self
    }
}

impl Ord for Bitmap {
    #[doc(alias = "hwloc_bitmap_compare")]
    fn cmp(&self, other: &Self) -> Ordering {
        let result = unsafe { ffi::hwloc_bitmap_compare(self.as_ptr(), other.as_ptr()) };
        match result {
            -1 => Ordering::Less,
            0 => Ordering::Equal,
            1 => Ordering::Greater,
            _ => unreachable!("hwloc_bitmap_compare returned unexpected result {result}"),
        }
    }
}

impl<B: Borrow<Bitmap>> PartialEq<B> for Bitmap {
    #[doc(alias = "hwloc_bitmap_isequal")]
    fn eq(&self, other: &B) -> bool {
        errors::call_hwloc_bool("hwloc_bitmap_isequal", || unsafe {
            ffi::hwloc_bitmap_isequal(self.as_ptr(), other.borrow().as_ptr())
        })
        .expect("Should not involve faillible syscalls")
    }
}

impl<B: Borrow<Bitmap>> PartialOrd<B> for Bitmap {
    fn partial_cmp(&self, other: &B) -> Option<Ordering> {
        Some(self.cmp(other.borrow()))
    }
}

unsafe impl Send for Bitmap {}

impl<B: Borrow<Bitmap>> Sub<B> for &Bitmap {
    type Output = Bitmap;

    #[doc(alias = "hwloc_bitmap_andnot")]
    fn sub(self, rhs: B) -> Bitmap {
        let mut result = Bitmap::new();
        errors::call_hwloc_int_normal("hwloc_bitmap_andnot", || unsafe {
            ffi::hwloc_bitmap_andnot(result.as_mut_ptr(), self.as_ptr(), rhs.borrow().as_ptr())
        })
        .expect("Bitmap operation failures are handled via panics");
        result
    }
}

impl<B: Borrow<Bitmap>> Sub<B> for Bitmap {
    type Output = Bitmap;

    fn sub(mut self, rhs: B) -> Bitmap {
        self -= rhs.borrow();
        self
    }
}

impl<B: Borrow<Bitmap>> SubAssign<B> for Bitmap {
    fn sub_assign(&mut self, rhs: B) {
        errors::call_hwloc_int_normal("hwloc_bitmap_andnot", || unsafe {
            ffi::hwloc_bitmap_andnot(self.as_mut_ptr(), self.as_ptr(), rhs.borrow().as_ptr())
        })
        .expect("Bitmap operation failures are handled via panics");
    }
}

unsafe impl Sync for Bitmap {}

/// Bitmap or a specialized form thereof
///
/// # Safety
///
/// Implementations of this type must effectively be a `repr(transparent)`
/// wrapper of `NonNull<RawBitmap>`, possibly with some ZSTs added.
#[doc(hidden)]
pub unsafe trait BitmapLike: Sealed {
    /// Access the inner `NonNull<RawBitmap>`
    fn as_raw(&self) -> NonNull<RawBitmap>;
}
//
impl Sealed for Bitmap {}
//
unsafe impl BitmapLike for Bitmap {
    fn as_raw(&self) -> NonNull<RawBitmap> {
        self.0
    }
}

/// Read-only reference to a [`Bitmap`]-like `Target` that is owned by hwloc
///
/// For most intents and purposes, you can think of this as an
/// `&'target Target` and use it as such. But it cannot literally be an
/// `&'target Target` due to annoying hwloc API technicalities...
#[repr(transparent)]
pub struct BitmapRef<'target, Target>(NonNull<RawBitmap>, PhantomData<&'target Target>);

impl<'target, Target: BitmapLike> BitmapRef<'target, Target> {
    /// Cast to another bitmap newtype
    pub fn cast<Other: BitmapLike>(self) -> BitmapRef<'target, Other> {
        BitmapRef(self.0, PhantomData)
    }
}

impl<'target, Target: BitmapLike> AsRef<Target> for BitmapRef<'target, Target> {
    fn as_ref(&self) -> &Target {
        // This is safe because...
        // - Both Target and BitmapRef are effectively repr(transparent)
        //   newtypes of NonNull<RawBitmap>, so &Target and &BitmapRef are
        //   effectively the same thing after compilation.
        // - The borrow checker ensures that one cannot construct an
        //   &'a BitmapRef<'target> which does not verify 'target: 'a, so one
        //   cannot use this AsRef impl to build an excessively long-lived
        //   &'a Target.
        unsafe { std::mem::transmute::<&BitmapRef<'target, Target>, &Target>(self) }
    }
}

impl<Target, Rhs> BitAnd<Rhs> for &BitmapRef<'_, Target>
where
    Target: BitmapLike,
    Rhs: Borrow<Target>,
    for<'a, 'b> &'a Target: BitAnd<&'b Target, Output = Target>,
{
    type Output = Target;

    fn bitand(self, rhs: Rhs) -> Target {
        self.as_ref() & rhs.borrow()
    }
}

impl<Target, Rhs> BitAnd<Rhs> for BitmapRef<'_, Target>
where
    Target: BitmapLike,
    Rhs: Borrow<Target>,
    for<'a, 'b> &'a Target: BitAnd<&'b Target, Output = Target>,
{
    type Output = Target;

    fn bitand(self, rhs: Rhs) -> Target {
        self.as_ref() & rhs.borrow()
    }
}

impl<Target, Rhs> BitOr<Rhs> for &BitmapRef<'_, Target>
where
    Target: BitmapLike,
    Rhs: Borrow<Target>,
    for<'a, 'b> &'a Target: BitOr<&'b Target, Output = Target>,
{
    type Output = Target;

    fn bitor(self, rhs: Rhs) -> Target {
        self.as_ref() | rhs.borrow()
    }
}

impl<Target, Rhs> BitOr<Rhs> for BitmapRef<'_, Target>
where
    Target: BitmapLike,
    Rhs: Borrow<Target>,
    for<'a, 'b> &'a Target: BitOr<&'b Target, Output = Target>,
{
    type Output = Target;

    fn bitor(self, rhs: Rhs) -> Target {
        self.as_ref() | rhs.borrow()
    }
}

impl<Target, Rhs> BitXor<Rhs> for &BitmapRef<'_, Target>
where
    Target: BitmapLike,
    Rhs: Borrow<Target>,
    for<'a, 'b> &'a Target: BitXor<&'b Target, Output = Target>,
{
    type Output = Target;

    fn bitxor(self, rhs: Rhs) -> Target {
        self.as_ref() ^ rhs.borrow()
    }
}

impl<Target, Rhs> BitXor<Rhs> for BitmapRef<'_, Target>
where
    Target: BitmapLike,
    Rhs: Borrow<Target>,
    for<'a, 'b> &'a Target: BitXor<&'b Target, Output = Target>,
{
    type Output = Target;

    fn bitxor(self, rhs: Rhs) -> Target {
        self.as_ref() ^ rhs.borrow()
    }
}

// NOTE: Needed to have impls of BitXyz<&BitmapRef> for Target
impl<Target: BitmapLike> Borrow<Target> for &BitmapRef<'_, Target> {
    fn borrow(&self) -> &Target {
        self.as_ref()
    }
}

impl<Target: BitmapLike> Borrow<Target> for BitmapRef<'_, Target> {
    fn borrow(&self) -> &Target {
        self.as_ref()
    }
}

impl<'target> Borrow<BitmapRef<'target, Bitmap>> for Bitmap {
    fn borrow(&self) -> &BitmapRef<'target, Bitmap> {
        // This is safe because...
        // - Bitmap and BitmapRef are effectively both repr(transparent)
        //   wrappers of NonNull<RawBitmap>, so they are layout-compatible.
        // - The borrow checker will not let us free the source Bitmap as long
        //   as the &BitmapRef emitted by this function exists.
        // - BitmapRef does not implement Clone, so it is not possible to create
        //   another BitmapRef that isn't covered by the above guarantee.
        unsafe { std::mem::transmute::<&Bitmap, &BitmapRef<'target, Bitmap>>(self) }
    }
}

// SAFETY: Do not implement Clone, or the Borrow impl above will open the door to UB.

impl<Target: BitmapLike + Debug> Debug for BitmapRef<'_, Target> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        <Target as Debug>::fmt(self.as_ref(), f)
    }
}

impl<Target: BitmapLike> Deref for BitmapRef<'_, Target> {
    type Target = Target;

    fn deref(&self) -> &Target {
        self.as_ref()
    }
}

impl<Target: BitmapLike + Display> Display for BitmapRef<'_, Target> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        <Target as Display>::fmt(self.as_ref(), f)
    }
}

impl<Target: BitmapLike + Eq + PartialEq<Self>> Eq for BitmapRef<'_, Target> {}

impl<'target, Target: BitmapLike> From<&'target Target> for BitmapRef<'target, Target> {
    fn from(input: &'target Target) -> Self {
        Self(input.as_raw(), PhantomData)
    }
}

impl<'target, 'self_, Target> IntoIterator for &'self_ BitmapRef<'target, Target>
where
    'target: 'self_,
    Target: BitmapLike,
    &'self_ Target: Borrow<Bitmap>,
{
    type Item = BitmapIndex;
    type IntoIter = BitmapIterator<&'self_ Target>;

    fn into_iter(self) -> Self::IntoIter {
        BitmapIterator::new(self.as_ref(), Bitmap::next_set)
    }
}

impl<'target, Target> IntoIterator for BitmapRef<'target, Target>
where
    Target: BitmapLike,
{
    type Item = BitmapIndex;
    type IntoIter = BitmapIterator<BitmapRef<'target, Bitmap>>;

    fn into_iter(self) -> Self::IntoIter {
        BitmapIterator::new(self.cast(), Bitmap::next_set)
    }
}

impl<Target> Not for &BitmapRef<'_, Target>
where
    Target: BitmapLike,
    for<'target> &'target Target: Not<Output = Target>,
{
    type Output = Target;

    fn not(self) -> Target {
        !(self.as_ref())
    }
}

impl<Target> Not for BitmapRef<'_, Target>
where
    Target: BitmapLike,
    for<'target> &'target Target: Not<Output = Target>,
{
    type Output = Target;

    fn not(self) -> Target {
        !(self.as_ref())
    }
}

impl<Target, Rhs> PartialEq<Rhs> for BitmapRef<'_, Target>
where
    Target: BitmapLike + PartialEq<Rhs>,
{
    fn eq(&self, other: &Rhs) -> bool {
        self.as_ref() == other
    }
}

impl<Target, Rhs> PartialOrd<Rhs> for BitmapRef<'_, Target>
where
    Target: BitmapLike + PartialOrd<Rhs>,
{
    fn partial_cmp(&self, other: &Rhs) -> Option<Ordering> {
        self.as_ref().partial_cmp(other)
    }
}

impl<Target: BitmapLike + Ord + PartialOrd<Self>> Ord for BitmapRef<'_, Target> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.as_ref().cmp(other.as_ref())
    }
}

unsafe impl<Target: BitmapLike + Send> Send for BitmapRef<'_, Target> {}

impl<Target, Rhs> Sub<Rhs> for &BitmapRef<'_, Target>
where
    Target: BitmapLike,
    Rhs: Borrow<Target>,
    for<'a, 'b> &'a Target: Sub<&'b Target, Output = Target>,
{
    type Output = Target;

    fn sub(self, rhs: Rhs) -> Target {
        self.as_ref() - rhs.borrow()
    }
}

impl<Target, Rhs> Sub<Rhs> for BitmapRef<'_, Target>
where
    Target: BitmapLike,
    Rhs: Borrow<Target>,
    for<'a, 'b> &'a Target: Sub<&'b Target, Output = Target>,
{
    type Output = Target;

    fn sub(self, rhs: Rhs) -> Target {
        self.as_ref() - rhs.borrow()
    }
}

unsafe impl<Target: BitmapLike + Sync> Sync for BitmapRef<'_, Target> {}

impl<'target, Target> ToOwned for BitmapRef<'target, Target>
where
    Target: BitmapLike + Borrow<BitmapRef<'target, Target>> + Clone,
{
    type Owned = Target;

    fn to_owned(&self) -> Target {
        self.as_ref().clone()
    }
}

/// Trait for manipulating specialized bitmaps (CpuSet, NodeSet) in a homogeneous way
pub trait SpecializedBitmap:
    AsRef<Bitmap>
    + AsMut<Bitmap>
    + BitmapLike
    + Clone
    + Debug
    + Display
    + From<Bitmap>
    + Into<Bitmap>
    + 'static
{
    /// What kind of bitmap is this?
    const BITMAP_KIND: BitmapKind;
}

/// Kind of specialized bitmap
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub enum BitmapKind {
    /// [`CpuSet`]
    CpuSet,

    /// [`NodeSet`]
    NodeSet,
}

/// Implement a specialized bitmap
#[macro_export]
#[doc(hidden)]
macro_rules! impl_bitmap_newtype {
    (
        $(#[$attr:meta])*
        $newtype:ident
    ) => {
        $(#[$attr])*
        #[derive(
            derive_more::AsMut,
            derive_more::AsRef,
            Clone,
            Default,
            Eq,
            derive_more::From,
            derive_more::Into,
            derive_more::IntoIterator,
            derive_more::Not,
            Ord,
        )]
        #[repr(transparent)]
        pub struct $newtype($crate::bitmaps::Bitmap);

        impl AsRef<$newtype> for $crate::bitmaps::Bitmap {
            fn as_ref(&self) -> &$newtype {
                // Safe because $newtype is repr(transparent)
                unsafe { std::mem::transmute(self) }
            }
        }

        impl $crate::bitmaps::SpecializedBitmap for $newtype {
            const BITMAP_KIND: $crate::bitmaps::BitmapKind =
                $crate::bitmaps::BitmapKind::$newtype;
        }

        /// # Re-export of the Bitmap API
        ///
        /// Only documentation headers are repeated here, you will find most of
        /// the documentation attached to identically named `Bitmap` methods.
        impl $newtype {
            /// Wraps an owned hwloc_bitmap_t
            ///
            /// See [`Bitmap::from_owned_raw_mut`](crate::bitmaps::Bitmap::from_owned_raw_mut).
            #[allow(unused)]
            pub(crate) unsafe fn from_owned_raw_mut(
                bitmap: *mut $crate::bitmaps::RawBitmap
            ) -> Option<Self> {
                $crate::bitmaps::Bitmap::from_owned_raw_mut(bitmap).map(Self::from)
            }

            /// Wraps an owned hwloc bitmap
            ///
            /// See [`Bitmap::from_owned_nonnull`](crate::bitmaps::Bitmap::from_owned_nonnull).
            #[allow(unused)]
            pub(crate) unsafe fn from_owned_nonnull(
                bitmap: std::ptr::NonNull<$crate::bitmaps::RawBitmap>
            ) -> Self {
                Self::from($crate::bitmaps::Bitmap::from_owned_nonnull(bitmap))
            }

            /// Wraps a borrowed hwloc_const_bitmap_t
            ///
            /// See [`Bitmap::borrow_from_raw`](crate::bitmaps::Bitmap::borrow_from_raw).
            #[allow(unused)]
            pub(crate) unsafe fn borrow_from_raw<'target>(
                bitmap: *const $crate::bitmaps::RawBitmap
            ) -> Option<$crate::bitmaps::BitmapRef<'target, Self>> {
                $crate::bitmaps::Bitmap::borrow_from_raw(bitmap)
                    .map(|bitmap_ref| bitmap_ref.cast())
            }

            /// Wraps a borrowed hwloc_bitmap_t
            ///
            /// See [`Bitmap::borrow_from_raw_mut`](crate::bitmaps::Bitmap::borrow_from_raw_mut).
            #[allow(unused)]
            pub(crate) unsafe fn borrow_from_raw_mut<'target>(
                bitmap: *mut $crate::bitmaps::RawBitmap
            ) -> Option<$crate::bitmaps::BitmapRef<'target, Self>> {
                $crate::bitmaps::Bitmap::borrow_from_raw_mut(bitmap)
                    .map(|bitmap_ref| bitmap_ref.cast())
            }

            /// Wraps a borrowed hwloc bitmap
            ///
            /// See [`Bitmap::borrow_from_nonnull`](crate::bitmaps::Bitmap::borrow_from_nonnull).
            #[allow(unused)]
            pub(crate) unsafe fn borrow_from_nonnull<'target>(
                bitmap: std::ptr::NonNull<$crate::bitmaps::RawBitmap>
            ) -> $crate::bitmaps::BitmapRef<'target, Self> {
                $crate::bitmaps::Bitmap::borrow_from_nonnull(bitmap).cast()
            }

            /// Contained bitmap pointer (for interaction with hwloc)
            ///
            /// See [`Bitmap::as_ptr`](crate::bitmaps::Bitmap::as_ptr).
            #[allow(unused)]
            pub(crate) fn as_ptr(&self) -> *const $crate::bitmaps::RawBitmap {
                self.0.as_ptr()
            }

            /// Contained mutable bitmap pointer (for interaction with hwloc)
            ///
            /// See [`Bitmap::as_mut_ptr`](crate::bitmaps::Bitmap::as_mut_ptr).
            #[allow(unused)]
            pub(crate) fn as_mut_ptr(&mut self) -> *mut $crate::bitmaps::RawBitmap {
                self.0.as_mut_ptr()
            }

            /// Create an empty bitmap
            ///
            /// See [`Bitmap::new`](crate::bitmaps::Bitmap::new).
            pub fn new() -> Self {
                Self::from($crate::bitmaps::Bitmap::new())
            }

            /// Create a full bitmap
            ///
            /// See [`Bitmap::full`](crate::bitmaps::Bitmap::full).
            pub fn full() -> Self {
                Self::from($crate::bitmaps::Bitmap::full())
            }

            /// Creates a new bitmap with the given range of indices set
            ///
            /// See [`Bitmap::from_range`](crate::bitmaps::Bitmap::from_range).
            pub fn from_range<Idx>(range: impl std::ops::RangeBounds<Idx>) -> Self
            where
                Idx: Copy + PartialEq + TryInto<$crate::bitmaps::BitmapIndex>,
                <Idx as TryInto<$crate::bitmaps::BitmapIndex>>::Error: std::fmt::Debug,
            {
                Self::from($crate::bitmaps::Bitmap::from_range(range))
            }

            /// Turn this bitmap into a copy of another bitmap
            ///
            /// See [`Bitmap::copy_from`](crate::bitmaps::Bitmap::copy_from).
            pub fn copy_from(&mut self, other: &Self) {
                self.0.copy_from(&other.0)
            }

            /// Clear all indices
            ///
            /// See [`Bitmap::clear`](crate::bitmaps::Bitmap::clear).
            pub fn clear(&mut self) {
                self.0.clear()
            }

            /// Set all indices
            ///
            /// See [`Bitmap::fill`](crate::bitmaps::Bitmap::fill).
            pub fn fill(&mut self) {
                self.0.fill()
            }

            /// Clear all indices except for `idx`, which is set
            ///
            /// See [`Bitmap::set_only`](crate::bitmaps::Bitmap::set_only).
            pub fn set_only<Idx>(&mut self, idx: Idx)
            where
                Idx: TryInto<$crate::bitmaps::BitmapIndex>,
                <Idx as TryInto<$crate::bitmaps::BitmapIndex>>::Error: std::fmt::Debug,
            {
                self.0.set_only(idx)
            }

            /// Set all indices except for `idx`, which is cleared
            ///
            /// See [`Bitmap::set_all_but`](crate::bitmaps::Bitmap::set_all_but).
            pub fn set_all_but<Idx>(&mut self, idx: Idx)
            where
                Idx: TryInto<$crate::bitmaps::BitmapIndex>,
                <Idx as TryInto<$crate::bitmaps::BitmapIndex>>::Error: std::fmt::Debug,
            {
                self.0.set_all_but(idx)
            }

            /// Set index `idx`
            ///
            /// See [`Bitmap::set`](crate::bitmaps::Bitmap::set).
            pub fn set<Idx>(&mut self, idx: Idx)
            where
                Idx: TryInto<$crate::bitmaps::BitmapIndex>,
                <Idx as TryInto<$crate::bitmaps::BitmapIndex>>::Error: std::fmt::Debug,
            {
                self.0.set(idx)
            }

            /// Set indices covered by `range`
            ///
            /// See [`Bitmap::set_range`](crate::bitmaps::Bitmap::set_range).
            pub fn set_range<Idx>(&mut self, range: impl std::ops::RangeBounds<Idx>)
            where
                Idx: Copy + PartialEq + TryInto<$crate::bitmaps::BitmapIndex>,
                <Idx as TryInto<$crate::bitmaps::BitmapIndex>>::Error: std::fmt::Debug,
            {
                self.0.set_range(range)
            }

            /// Clear index `idx`
            ///
            /// See [`Bitmap::unset`](crate::bitmaps::Bitmap::unset).
            pub fn unset<Idx>(&mut self, idx: Idx)
            where
                Idx: TryInto<$crate::bitmaps::BitmapIndex>,
                <Idx as TryInto<$crate::bitmaps::BitmapIndex>>::Error: std::fmt::Debug,
            {
                self.0.unset(idx)
            }

            /// Clear indices covered by `range`
            ///
            /// See [`Bitmap::unset_range`](crate::bitmaps::Bitmap::unset_range).
            pub fn unset_range<Idx>(&mut self, range: impl std::ops::RangeBounds<Idx>)
            where
                Idx: Copy + PartialEq + TryInto<$crate::bitmaps::BitmapIndex>,
                <Idx as TryInto<$crate::bitmaps::BitmapIndex>>::Error: std::fmt::Debug,
            {
                self.0.unset_range(range)
            }

            /// Keep a single index among those set in the bitmap
            ///
            /// See [`Bitmap::singlify`](crate::bitmaps::Bitmap::singlify).
            pub fn singlify(&mut self) {
                self.0.singlify()
            }

            /// Check if index `idx` is set
            ///
            /// See [`Bitmap::is_set`](crate::bitmaps::Bitmap::is_set).
            pub fn is_set<Idx>(&self, idx: Idx) -> bool
            where
                Idx: TryInto<$crate::bitmaps::BitmapIndex>,
                <Idx as TryInto<$crate::bitmaps::BitmapIndex>>::Error: std::fmt::Debug,
            {
                self.0.is_set(idx)
            }

            /// Check if all indices are unset
            ///
            /// See [`Bitmap::is_empty`](crate::bitmaps::Bitmap::is_empty).
            pub fn is_empty(&self) -> bool {
                self.0.is_empty()
            }

            /// Check if all indices are set
            ///
            /// See [`Bitmap::is_full`](crate::bitmaps::Bitmap::is_full).
            pub fn is_full(&self) -> bool {
                self.0.is_full()
            }

            /// Check the first set index, if any
            ///
            /// See [`Bitmap::first_set`](crate::bitmaps::Bitmap::first_set).
            pub fn first_set(&self) -> Option<$crate::bitmaps::BitmapIndex> {
                self.0.first_set()
            }

            /// Iterate over set indices
            ///
            /// See [`Bitmap::iter_set`](crate::bitmaps::Bitmap::iter_set).
            pub fn iter_set(
                &self
            ) -> $crate::bitmaps::BitmapIterator<&$crate::bitmaps::Bitmap> {
                self.0.iter_set()
            }

            /// Check the last set index, if any
            ///
            /// See [`Bitmap::last_set`](crate::bitmaps::Bitmap::last_set).
            pub fn last_set(&self) -> Option<$crate::bitmaps::BitmapIndex> {
                self.0.last_set()
            }

            /// The number of indices that are set in the bitmap.
            ///
            /// See [`Bitmap::weight`](crate::bitmaps::Bitmap::weight).
            pub fn weight(&self) -> Option<usize> {
                self.0.weight()
            }

            /// Check the first unset index, if any
            ///
            /// See [`Bitmap::first_unset`](crate::bitmaps::Bitmap::first_unset).
            pub fn first_unset(&self) -> Option<$crate::bitmaps::BitmapIndex> {
                self.0.first_unset()
            }

            /// Iterate over unset indices
            ///
            /// See [`Bitmap::iter_unset`](crate::bitmaps::Bitmap::iter_unset).
            pub fn iter_unset(
                &self
            ) -> $crate::bitmaps::BitmapIterator<&$crate::bitmaps::Bitmap> {
                self.0.iter_unset()
            }

            /// Check the last unset index, if any
            ///
            /// See [`Bitmap::last_unset`](crate::bitmaps::Bitmap::last_unset).
            pub fn last_unset(&self) -> Option<$crate::bitmaps::BitmapIndex> {
                self.0.last_unset()
            }

            /// Inverts the current `Bitmap`.
            ///
            /// See [`Bitmap::invert`](crate::bitmaps::Bitmap::invert).
            pub fn invert(&mut self) {
                self.0.invert()
            }

            /// Truth that `self` and `rhs` have some set indices in common
            ///
            /// See [`Bitmap::intersects`](crate::bitmaps::Bitmap::intersects).
            pub fn intersects(&self, rhs: &Self) -> bool {
                self.0.intersects(&rhs.0)
            }

            /// Truth that the indices set in `inner` are a subset of those set in `self`
            ///
            /// See [`Bitmap::includes`](crate::bitmaps::Bitmap::includes).
            pub fn includes(&self, inner: &Self) -> bool {
                self.0.includes(&inner.0)
            }
        }

        unsafe impl $crate::bitmaps::BitmapLike for $newtype {
            fn as_raw(&self) -> std::ptr::NonNull<$crate::bitmaps::RawBitmap> {
                self.0.as_raw()
            }
        }

        impl<B: std::borrow::Borrow<$newtype>> std::ops::BitAnd<B> for &$newtype {
            type Output = $newtype;

            fn bitand(self, rhs: B) -> $newtype {
                $newtype((&self.0) & (&rhs.borrow().0))
            }
        }

        impl<B: std::borrow::Borrow<$newtype>> std::ops::BitAnd<B> for $newtype {
            type Output = $newtype;

            fn bitand(self, rhs: B) -> $newtype {
                $newtype(self.0 & (&rhs.borrow().0))
            }
        }

        impl<B: std::borrow::Borrow<$newtype>> std::ops::BitAndAssign<B> for $newtype {
            fn bitand_assign(&mut self, rhs: B) {
                self.0 &= (&rhs.borrow().0)
            }
        }

        impl<B: std::borrow::Borrow<$newtype>> std::ops::BitOr<B> for &$newtype {
            type Output = $newtype;

            fn bitor(self, rhs: B) -> $newtype {
                $newtype(&self.0 | &rhs.borrow().0)
            }
        }

        impl<B: std::borrow::Borrow<$newtype>> std::ops::BitOr<B> for $newtype {
            type Output = $newtype;

            fn bitor(self, rhs: B) -> $newtype {
                $newtype(self.0 | &rhs.borrow().0)
            }
        }

        impl<B: std::borrow::Borrow<$newtype>> std::ops::BitOrAssign<B> for $newtype {
            fn bitor_assign(&mut self, rhs: B) {
                self.0 |= &rhs.borrow().0
            }
        }

        impl<B: std::borrow::Borrow<$newtype>> std::ops::BitXor<B> for &$newtype {
            type Output = $newtype;

            fn bitxor(self, rhs: B) -> $newtype {
                $newtype(&self.0 ^ &rhs.borrow().0)
            }
        }

        impl<B: std::borrow::Borrow<$newtype>> std::ops::BitXor<B> for $newtype {
            type Output = $newtype;

            fn bitxor(self, rhs: B) -> $newtype {
                $newtype(self.0 ^ &rhs.borrow().0)
            }
        }

        impl<B: std::borrow::Borrow<$newtype>> std::ops::BitXorAssign<B> for $newtype {
            fn bitxor_assign(&mut self, rhs: B) {
                self.0 ^= &rhs.borrow().0
            }
        }

        impl<'target> std::borrow::Borrow<$crate::bitmaps::BitmapRef<'target, $newtype>> for $newtype {
            fn borrow(&self) -> &$crate::bitmaps::BitmapRef<'target, $newtype> {
                // This is safe because...
                // - $newtype and BitmapRef are effectively both repr(transparent)
                //   wrappers of NonNull<RawBitmap>, so they are layout-compatible.
                // - The borrow checker will not let us free the source $newtype as long
                //   as the &BitmapRef emitted by this function exists.
                // - BitmapRef does not implement Clone, so it is not possible to create
                //   another BitmapRef that isn't covered by the above guarantee.
                unsafe { std::mem::transmute::<&$newtype, &$crate::bitmaps::BitmapRef<'target, $newtype>>(self) }
            }
        }

        impl std::fmt::Debug for $newtype {
            fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(f, "{}({:?})", stringify!($newtype), &self.0)
            }
        }

        impl std::fmt::Display for $newtype {
            fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(f, "{}({})", stringify!($newtype), &self.0)
            }
        }

        impl<BI: std::borrow::Borrow<$crate::bitmaps::BitmapIndex>> Extend<BI> for $newtype {
            fn extend<T: IntoIterator<Item = BI>>(&mut self, iter: T) {
                self.0.extend(iter)
            }
        }

        impl<BI: std::borrow::Borrow<$crate::bitmaps::BitmapIndex>> From<BI> for $newtype {
            fn from(value: BI) -> Self {
                Self(value.into())
            }
        }

        impl<BI: std::borrow::Borrow<$crate::bitmaps::BitmapIndex>> FromIterator<BI> for $newtype {
            fn from_iter<I: IntoIterator<Item = BI>>(iter: I) -> Self {
                Self($crate::bitmaps::Bitmap::from_iter(iter))
            }
        }

        impl<'newtype> IntoIterator for &'newtype $newtype {
            type Item = $crate::bitmaps::BitmapIndex;
            type IntoIter = $crate::bitmaps::BitmapIterator<&'newtype $crate::bitmaps::Bitmap>;

            fn into_iter(self) -> Self::IntoIter {
                (&self.0).into_iter()
            }
        }

        impl std::ops::Not for &$newtype {
            type Output = $newtype;

            fn not(self) -> $newtype {
                $newtype(!&self.0)
            }
        }

        impl<B: std::borrow::Borrow<$newtype>> PartialEq<B> for $newtype {
            fn eq(&self, other: &B) -> bool {
                self.0 == other.borrow().0
            }
        }

        impl<B: std::borrow::Borrow<$newtype>> PartialOrd<B> for $newtype {
            fn partial_cmp(&self, other: &B) -> Option<std::cmp::Ordering> {
                self.0.partial_cmp(&other.borrow().0)
            }
        }

        impl $crate::Sealed for $newtype {}

        impl<B: std::borrow::Borrow<$newtype>> std::ops::Sub<B> for &$newtype {
            type Output = $newtype;

            fn sub(self, rhs: B) -> $newtype {
                $newtype(&self.0 - &rhs.borrow().0)
            }
        }

        impl<B: std::borrow::Borrow<$newtype>> std::ops::Sub<B> for $newtype {
            type Output = $newtype;

            fn sub(self, rhs: B) -> $newtype {
                $newtype(self.0 - &rhs.borrow().0)
            }
        }

        impl<B: std::borrow::Borrow<$newtype>> std::ops::SubAssign<B> for $newtype {
            fn sub_assign(&mut self, rhs: B) {
                self.0 -= &rhs.borrow().0
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use quickcheck_macros::quickcheck;
    use std::{
        collections::HashSet,
        ffi::c_ulonglong,
        fmt::Write,
        ops::{Range, RangeFrom, RangeInclusive},
    };

    // We can't fully check the value of infinite iterators because that would
    // literally take forever, so we only check a small subrange of the final
    // all-set/unset region, large enough to catch off-by-one-longword issues.
    const INFINITE_EXPLORE_ITERS: usize = std::mem::size_of::<c_ulonglong>() * 8;

    // Unfortunately, ranges of BitmapIndex cannot do everything that ranges of
    // built-in integer types can do due to some unstable integer traits, so
    // it's sometimes good to go back to usize.
    fn range_inclusive_to_usize(range: &RangeInclusive<BitmapIndex>) -> RangeInclusive<usize> {
        usize::from(*range.start())..=usize::from(*range.end())
    }

    // Split a possibly infinite bitmap into a finite bitmap and an infinite
    // range of set indices, separated from the indices of the finite bitmap by
    // a range of unset indices. To get the original bitmap back, use `set_range`.
    fn split_infinite_bitmap(mut bitmap: Bitmap) -> (Bitmap, Option<RangeFrom<BitmapIndex>>) {
        // If this bitmap is infinite...
        if bitmap.weight().is_none() {
            // ...and it has a finite part...
            if let Some(last_unset) = bitmap.last_unset() {
                let infinite_part = last_unset.checked_succ().unwrap()..;
                bitmap.unset_range(infinite_part.clone());
                (bitmap, Some(infinite_part))
            } else {
                (Bitmap::new(), Some(BitmapIndex::MIN..))
            }
        } else {
            (bitmap, None)
        }
    }

    fn test_basic_inplace(initial: &Bitmap, inverse: &Bitmap) {
        let mut buf = initial.clone();
        buf.clear();
        assert!(buf.is_empty());

        buf.copy_from(initial);
        buf.fill();
        assert!(buf.is_full());

        buf.copy_from(initial);
        buf.invert();
        assert_eq!(buf, *inverse);

        if initial.weight().unwrap_or(usize::MAX) > 0 {
            buf.copy_from(initial);
            buf.singlify();
            assert_eq!(buf.weight(), Some(1));
        }
    }

    fn test_indexing(initial: &Bitmap, index: BitmapIndex, initially_set: bool) {
        let single = Bitmap::from(index);
        let single_hole = !&single;

        // Bitmaps are conceptually infinite so we must be careful with
        // iteration-based verification of full bitmap contents. Let's just
        // account for off-by-one-word errors.
        let max_iters = initial
            .weight()
            .unwrap_or(usize::from(index) + INFINITE_EXPLORE_ITERS);

        assert_eq!(initial.is_set(index), initially_set);

        let mut buf = initial.clone();
        buf.set(index);
        assert_eq!(
            buf.weight(),
            initial.weight().map(|w| w + !initially_set as usize)
        );
        for idx in std::iter::once(index).chain(initial.iter_set().take(max_iters)) {
            assert!(buf.is_set(idx));
        }

        buf.copy_from(initial);
        buf.set_only(index);
        assert_eq!(buf, single);

        buf.copy_from(initial);
        buf.set_all_but(index);
        assert_eq!(buf, single_hole);

        buf.copy_from(initial);
        buf.unset(index);
        assert_eq!(
            buf.weight(),
            initial.weight().map(|w| w - initially_set as usize)
        );
        for idx in initial.iter_set().take(max_iters) {
            assert_eq!(buf.is_set(idx), idx != index);
        }
    }

    fn test_and_sub(b1: &Bitmap, b2: &Bitmap, and: &Bitmap) {
        assert_eq!(b1 & b2, *and);
        let mut buf = b1.clone();
        buf &= b2;
        assert_eq!(buf, *and);

        let b1_andnot_b2 = b1 & !b2;
        assert_eq!(b1 - b2, b1_andnot_b2);
        buf.copy_from(b1);
        buf -= b2;
        assert_eq!(buf, b1_andnot_b2);

        let b2_andnot_b1 = b2 & !b1;
        assert_eq!(b2 - b1, b2_andnot_b1);
        buf.copy_from(b2);
        buf -= b1;
        assert_eq!(buf, b2_andnot_b1);
    }

    #[allow(clippy::redundant_clone)]
    #[test]
    fn empty() {
        let empty = Bitmap::new();
        let inverse = Bitmap::full();

        let test_empty = |empty: &Bitmap| {
            assert_eq!(empty.first_set(), None);
            assert_eq!(empty.first_unset().map(usize::from), Some(0));
            assert!(empty.is_empty());
            assert!(!empty.is_full());
            assert_eq!(empty.into_iter().count(), 0);
            assert_eq!(empty.iter_set().count(), 0);
            assert_eq!(empty.last_set(), None);
            assert_eq!(empty.last_unset(), None);
            assert_eq!(empty.weight(), Some(0));

            for (expected, idx) in empty.iter_unset().enumerate().take(INFINITE_EXPLORE_ITERS) {
                assert_eq!(expected, usize::from(idx));
            }

            assert_eq!(format!("{empty:?}"), "");
            assert_eq!(format!("{empty}"), "");
            assert_eq!(!empty, inverse);
        };
        test_empty(&empty);
        test_empty(&empty.clone());
        test_empty(&Bitmap::default());

        test_basic_inplace(&empty, &inverse);
    }

    #[quickcheck]
    fn empty_extend(extra: HashSet<BitmapIndex>) {
        let mut extended = Bitmap::new();
        extended.extend(extra.iter().copied());

        assert_eq!(extended.weight(), Some(extra.len()));
        for idx in extra {
            assert!(extended.is_set(idx));
        }
    }

    #[quickcheck]
    fn empty_op_index(index: BitmapIndex) {
        test_indexing(&Bitmap::new(), index, false);
    }

    #[quickcheck]
    fn empty_op_range(range: Range<BitmapIndex>) {
        let mut buf = Bitmap::new();
        buf.set_range(range.clone());
        assert_eq!(buf, Bitmap::from_range(range.clone()));
        buf.clear();

        buf.unset_range(range);
        assert!(buf.is_empty());
    }

    #[quickcheck]
    fn empty_op_bitmap(other: Bitmap) {
        let empty = Bitmap::new();

        assert_eq!(empty.includes(&other), other.is_empty());
        assert!(other.includes(&empty));
        assert!(!empty.intersects(&other));

        assert_eq!(empty == other, other.is_empty());
        if !other.is_empty() {
            assert!(empty < other);
        }

        test_and_sub(&empty, &other, &empty);

        assert_eq!(&empty | &other, other);
        let mut buf = Bitmap::new();
        buf |= &other;
        assert_eq!(buf, other);

        assert_eq!(&empty ^ &other, other);
        buf.clear();
        buf ^= &other;
        assert_eq!(buf, other);
    }

    #[allow(clippy::redundant_clone)]
    #[test]
    fn full() {
        let full = Bitmap::full();
        let inverse = Bitmap::new();

        let test_full = |full: &Bitmap| {
            assert_eq!(full.first_set().map(usize::from), Some(0));
            assert_eq!(full.first_unset(), None);
            assert!(!full.is_empty());
            assert!(full.is_full());
            assert_eq!(full.iter_unset().count(), 0);
            assert_eq!(full.last_set(), None);
            assert_eq!(full.last_unset(), None);
            assert_eq!(full.weight(), None);

            fn test_iter_set(iter: impl Iterator<Item = BitmapIndex>) {
                for (expected, idx) in iter.enumerate().take(INFINITE_EXPLORE_ITERS) {
                    assert_eq!(expected, usize::from(idx));
                }
            }
            test_iter_set(full.into_iter());
            test_iter_set(full.iter_set());

            assert_eq!(format!("{full:?}"), "0-");
            assert_eq!(format!("{full}"), "0-");
            assert_eq!(!full, inverse);
        };
        test_full(&full);
        test_full(&full.clone());

        test_basic_inplace(&full, &inverse);
    }

    #[quickcheck]
    fn full_extend(extra: HashSet<BitmapIndex>) {
        let mut extended = Bitmap::full();
        extended.extend(extra.iter().copied());
        assert!(extended.is_full());
    }

    #[quickcheck]
    fn full_op_index(index: BitmapIndex) {
        test_indexing(&Bitmap::full(), index, true);
    }

    #[quickcheck]
    fn full_op_range(range: Range<BitmapIndex>) {
        let mut ranged_hole = Bitmap::from_range(range.clone());
        ranged_hole.invert();

        let mut buf = Bitmap::full();
        buf.set_range(range.clone());
        assert!(buf.is_full());

        buf.fill();
        buf.unset_range(range);
        assert_eq!(buf, ranged_hole);
    }

    #[quickcheck]
    fn full_op_bitmap(other: Bitmap) {
        let full = Bitmap::full();
        let not_other = !&other;

        assert!(full.includes(&other));
        assert_eq!(other.includes(&full), other.is_full());
        assert_eq!(full.intersects(&other), !other.is_empty());

        assert_eq!(full == other, other.is_full());
        assert_eq!(
            full.cmp(&other),
            if other.is_full() {
                Ordering::Equal
            } else {
                Ordering::Greater
            }
        );

        test_and_sub(&full, &other, &other);

        assert!((&full | &other).is_full());
        let mut buf = Bitmap::full();
        buf |= &other;
        assert!(buf.is_full());

        assert_eq!(&full ^ &other, not_other);
        buf.fill();
        buf ^= &other;
        assert_eq!(buf, not_other);
    }

    #[allow(clippy::redundant_clone)]
    #[quickcheck]
    fn from_range(range: RangeInclusive<BitmapIndex>) {
        let ranged_bitmap = Bitmap::from_range(range.clone());

        let elems = (usize::from(*range.start())..=usize::from(*range.end()))
            .map(|idx| BitmapIndex::try_from(idx).unwrap())
            .collect::<Vec<_>>();
        let first_unset = if let Some(&BitmapIndex::MIN) = elems.first() {
            elems.last().copied().and_then(BitmapIndex::checked_succ)
        } else {
            Some(BitmapIndex::MIN)
        };
        let unset_after_set = if let Some(last_set) = elems.last() {
            last_set.checked_succ()
        } else {
            Some(BitmapIndex::MIN)
        };
        let display = if let (Some(first), Some(last)) = (elems.first(), elems.last()) {
            if first != last {
                format!("{first}-{last}")
            } else {
                format!("{first}")
            }
        } else {
            String::new()
        };
        let inverse = if let (Some(&first), Some(last)) = (elems.first(), elems.last()) {
            let mut buf = Bitmap::from_range(..first);
            if let Some(after_last) = last.checked_succ() {
                buf.set_range(after_last..)
            }
            buf
        } else {
            Bitmap::full()
        };

        let test_ranged = |ranged_bitmap: &Bitmap| {
            assert_eq!(ranged_bitmap.first_set(), elems.first().copied());
            assert_eq!(ranged_bitmap.first_unset(), first_unset);
            assert_eq!(ranged_bitmap.is_empty(), elems.is_empty());
            assert!(!ranged_bitmap.is_full());
            assert_eq!(ranged_bitmap.into_iter().collect::<Vec<_>>(), elems);
            assert_eq!(ranged_bitmap.iter_set().collect::<Vec<_>>(), elems);
            assert_eq!(ranged_bitmap.last_set(), elems.last().copied());
            assert_eq!(ranged_bitmap.last_unset(), None);
            assert_eq!(ranged_bitmap.weight(), Some(elems.len()));

            let mut unset = ranged_bitmap.iter_unset();
            if let Some(first_set) = elems.first() {
                for expected_unset in 0..usize::from(*first_set) {
                    assert_eq!(unset.next().map(usize::from), Some(expected_unset));
                }
            }
            let mut expected_unset =
                std::iter::successors(unset_after_set, |unset| unset.checked_succ());
            for unset_index in unset.take(INFINITE_EXPLORE_ITERS) {
                assert_eq!(unset_index, expected_unset.next().unwrap())
            }

            assert_eq!(format!("{ranged_bitmap:?}"), display);
            assert_eq!(format!("{ranged_bitmap}"), display);
            assert_eq!(!ranged_bitmap, inverse);
        };
        test_ranged(&ranged_bitmap);
        test_ranged(&ranged_bitmap.clone());

        test_basic_inplace(&ranged_bitmap, &inverse);
    }

    #[quickcheck]
    fn from_range_extend(range: RangeInclusive<BitmapIndex>, extra: HashSet<BitmapIndex>) {
        let mut extended = Bitmap::from_range(range.clone());
        let mut indices = extra.clone();
        extended.extend(extra);

        for idx in usize::from(*range.start())..=usize::from(*range.end()) {
            indices.insert(idx.try_into().unwrap());
        }

        assert_eq!(extended.weight(), Some(indices.len()));
        for idx in indices {
            assert!(extended.is_set(idx));
        }
    }

    #[quickcheck]
    fn from_range_op_index(range: RangeInclusive<BitmapIndex>, index: BitmapIndex) {
        test_indexing(
            &Bitmap::from_range(range.clone()),
            index,
            range.contains(&index),
        );
    }

    #[quickcheck]
    fn from_range_op_range(
        range: RangeInclusive<BitmapIndex>,
        other_range: RangeInclusive<BitmapIndex>,
    ) {
        let usized = range_inclusive_to_usize(&range);
        let other_usized = range_inclusive_to_usize(&other_range);

        let num_indices = |range: &RangeInclusive<usize>| range.clone().count();
        let num_common_indices = if usized.is_empty() || other_usized.is_empty() {
            0
        } else {
            num_indices(
                &(*usized.start().max(other_usized.start())
                    ..=*usized.end().min(other_usized.end())),
            )
        };

        let ranged_bitmap = Bitmap::from_range(range);

        let mut buf = ranged_bitmap.clone();
        buf.set_range(other_range.clone());
        assert_eq!(
            buf.weight().unwrap(),
            num_indices(&usized) + num_indices(&other_usized) - num_common_indices
        );
        for idx in usized.clone().chain(other_usized.clone()) {
            assert!(buf.is_set(idx));
        }

        buf.copy_from(&ranged_bitmap);
        buf.unset_range(other_range);
        assert_eq!(
            buf.weight().unwrap(),
            num_indices(&usized) - num_common_indices
        );
        for idx in usized {
            assert_eq!(buf.is_set(idx), !other_usized.contains(&idx));
        }
    }

    #[quickcheck]
    fn from_range_op_bitmap(range: RangeInclusive<BitmapIndex>, other: Bitmap) {
        let ranged_bitmap = Bitmap::from_range(range.clone());
        let usized = range_inclusive_to_usize(&range);

        assert_eq!(
            ranged_bitmap.includes(&other),
            other.is_empty()
                || (other.last_set().is_some() && other.iter_set().all(|idx| range.contains(&idx)))
        );
        assert_eq!(
            other.includes(&ranged_bitmap),
            usized.clone().all(|idx| other.is_set(idx))
        );
        assert_eq!(
            ranged_bitmap.intersects(&other),
            usized.clone().any(|idx| other.is_set(idx))
        );

        assert_eq!(
            ranged_bitmap == other,
            other.weight() == Some(usized.count()) && other.includes(&ranged_bitmap)
        );

        if ranged_bitmap.is_empty() {
            assert_eq!(
                ranged_bitmap.cmp(&other),
                if !other.is_empty() {
                    Ordering::Less
                } else {
                    Ordering::Equal
                }
            );
        } else {
            match ranged_bitmap.cmp(&other) {
                Ordering::Less => {
                    assert!(
                        other.last_set().unwrap_or(BitmapIndex::MAX) > *range.end()
                            || (other.includes(&ranged_bitmap)
                                && other.first_set().unwrap_or(BitmapIndex::MIN) < *range.start())
                    )
                }
                Ordering::Equal => assert_eq!(ranged_bitmap, other),
                Ordering::Greater => assert!(!other.includes(&ranged_bitmap)),
            }
        }

        let (other_finite, other_infinite) = split_infinite_bitmap(other.clone());

        let mut ranged_and_other = other_finite
            .iter_set()
            .filter(|idx| range.contains(idx))
            .collect::<Bitmap>();
        if let Some(infinite) = &other_infinite {
            if !ranged_bitmap.is_empty() {
                ranged_and_other.set_range(infinite.start.max(*range.start())..=*range.end());
            }
        }
        test_and_sub(&ranged_bitmap, &other, &ranged_and_other);

        let mut ranged_or_other = other.clone();
        ranged_or_other.set_range(range);
        assert_eq!(&ranged_bitmap | &other, ranged_or_other);
        let mut buf = ranged_bitmap.clone();
        buf |= &other;
        assert_eq!(buf, ranged_or_other);

        let ranged_xor_other = ranged_or_other - ranged_and_other;
        assert_eq!(&ranged_bitmap ^ &other, ranged_xor_other);
        let mut buf = ranged_bitmap;
        buf ^= &other;
        assert_eq!(buf, ranged_xor_other);
    }

    #[quickcheck]
    fn from_iterator(indices: HashSet<BitmapIndex>) {
        let bitmap = indices.iter().copied().collect::<Bitmap>();
        assert_eq!(bitmap.weight(), Some(indices.len()));
        for idx in indices {
            assert!(bitmap.is_set(idx));
        }
    }

    #[allow(clippy::redundant_clone)]
    #[quickcheck]
    fn arbitrary(bitmap: Bitmap) {
        // Test properties pertaining to first iterator output
        assert_eq!(bitmap.first_set(), bitmap.iter_set().next());
        assert_eq!(bitmap.first_unset(), bitmap.iter_unset().next());
        assert_eq!(bitmap.is_empty(), bitmap.first_set().is_none());
        assert_eq!(bitmap.is_full(), bitmap.first_unset().is_none());

        // Test iterator-wide properties
        fn test_iter(
            bitmap: &Bitmap,
            iter_set: impl Iterator<Item = BitmapIndex>,
        ) -> (Bitmap, String) {
            let mut iter_set = iter_set.peekable();
            let mut iter_unset = bitmap.iter_unset().peekable();

            // Iterate over BitmapIndex until the end ot either iterator is reached
            let mut next_index = BitmapIndex::MIN;
            let mut set_stripe_start = None;
            let mut observed_weight = 0;
            let mut observed_last_set = None;
            let mut observed_last_unset = None;
            let mut inverse = Bitmap::full();
            let mut display = String::new();
            //
            while let (Some(next_set), Some(next_unset)) =
                (iter_set.peek().copied(), iter_unset.peek().copied())
            {
                // Move least advanced iterator forward
                match next_set.cmp(&next_unset) {
                    Ordering::Less => {
                        // Next index should be set
                        iter_set.next();
                        assert_eq!(next_set, next_index);

                        // Acknowledge that a set index has been processed
                        observed_last_set = Some(next_set);
                        observed_weight += 1;
                        if set_stripe_start.is_none() {
                            set_stripe_start = Some(next_set);
                        }
                    }
                    // Next index should be unset
                    Ordering::Greater => {
                        // Next index should be unset
                        iter_unset.next();
                        assert_eq!(next_unset, next_index);
                        observed_last_unset = Some(next_unset);

                        // If we just went through a stripe of set indices,
                        // propagate that into the inverse & display predictions
                        if let Some(first_set) = set_stripe_start {
                            let last_set = observed_last_set.unwrap();
                            inverse.unset_range(first_set..=last_set);
                            if !display.is_empty() {
                                write!(display, ",").unwrap();
                            }
                            write!(display, "{first_set}").unwrap();
                            if last_set != first_set {
                                write!(display, "-{last_set}").unwrap();
                            }
                            set_stripe_start = None;
                        }
                    }
                    Ordering::Equal => unreachable!("Next index can't be both set and unset"),
                }

                // Update next_index
                next_index = next_index.checked_succ().expect(
                    "Shouldn't overflow if we had both a next set & unset index before iterating",
                );
            }

            // At this point, we reached the end of one of the iterators, and
            // the other iterator should just keep producing an infinite
            // sequence of consecutive indices. Reach some conclusions...
            let mut infinite_iter: Box<dyn Iterator<Item = BitmapIndex>> =
                match (iter_set.peek(), iter_unset.peek()) {
                    (Some(next_set), None) => {
                        // Check end-of-iterator properties
                        assert_eq!(bitmap.last_set(), None);
                        assert_eq!(bitmap.last_unset(), observed_last_unset);
                        assert_eq!(bitmap.weight(), None);

                        // Handle last (infinite) range of set elements
                        let stripe_start = set_stripe_start.unwrap_or(*next_set);
                        inverse.unset_range(stripe_start..);
                        if !display.is_empty() {
                            write!(display, ",").unwrap();
                        }
                        write!(display, "{stripe_start}-").unwrap();

                        // Expose infinite iterator of set elements
                        Box::new(iter_set)
                    }
                    (None, Some(_unset)) => {
                        // Check end-of-iterator properties
                        assert_eq!(bitmap.last_set(), observed_last_set);
                        assert_eq!(bitmap.last_unset(), None);
                        assert_eq!(bitmap.weight(), Some(observed_weight));

                        // Handle previous range of set elements, if any
                        if let Some(first_set) = set_stripe_start {
                            let last_set = observed_last_set.unwrap();
                            inverse.unset_range(first_set..=last_set);
                            if !display.is_empty() {
                                write!(display, ",").unwrap();
                            }
                            write!(display, "{first_set}").unwrap();
                            if last_set != first_set {
                                write!(display, "-{last_set}").unwrap();
                            }
                        }

                        Box::new(iter_unset)
                    }
                    _ => unreachable!("At least one iterator is finite, they can't both be"),
                };

            // ...and iterate the infinite iterator for a while to check it
            // does seem to meet expectations.
            for _ in 0..INFINITE_EXPLORE_ITERS {
                assert_eq!(infinite_iter.next(), Some(next_index));
                if let Some(index) = next_index.checked_succ() {
                    next_index = index;
                } else {
                    break;
                }
            }

            // Return predicted bitmap inverse and display
            (inverse, display)
        }
        let (inverse, display) = test_iter(&bitmap, (&bitmap).into_iter());
        let (inverse2, display2) = test_iter(&bitmap, bitmap.iter_set());
        //
        assert_eq!(inverse, inverse2);
        assert_eq!(display, display2);
        assert_eq!(!&bitmap, inverse);
        assert_eq!(format!("{bitmap:?}"), display);
        assert_eq!(format!("{bitmap}"), display);

        // Test in-place operations
        test_basic_inplace(&bitmap, &inverse);

        // Test that a clone is indistinguishable from the original bitmap
        let clone = bitmap.clone();
        assert_eq!(clone.first_set(), bitmap.first_set());
        assert_eq!(clone.first_unset(), bitmap.first_unset());
        assert_eq!(clone.is_empty(), bitmap.is_empty());
        assert_eq!(clone.is_full(), bitmap.is_full());
        assert_eq!(clone.last_set(), bitmap.last_set());
        assert_eq!(clone.last_unset(), bitmap.last_unset());
        assert_eq!(clone.weight(), bitmap.weight());
        //
        let (finite, infinite) = split_infinite_bitmap(bitmap);
        if let Some(infinite) = infinite {
            let test_iter = |mut iter_set: Box<dyn Iterator<Item = BitmapIndex>>| {
                let mut iter_unset = clone.iter_unset().fuse();
                let infinite_start = usize::from(infinite.start);
                for idx in 0..infinite_start {
                    let next = if finite.is_set(idx) {
                        iter_set.next()
                    } else {
                        iter_unset.next()
                    };
                    assert_eq!(next.map(usize::from), Some(idx));
                }
                assert_eq!(iter_unset.next(), None);
                for idx in (infinite_start..).take(INFINITE_EXPLORE_ITERS) {
                    assert_eq!(iter_set.next().map(usize::from), Some(idx));
                }
            };
            test_iter(Box::new((&clone).into_iter()));
            test_iter(Box::new(clone.iter_set()));
        } else {
            assert_eq!((&clone).into_iter().collect::<Bitmap>(), finite);
            assert_eq!(clone.iter_set().collect::<Bitmap>(), finite);

            let num_iters = usize::from(finite.last_set().unwrap_or(BitmapIndex::MIN)) + 1
                - finite.weight().unwrap()
                + INFINITE_EXPLORE_ITERS;
            let mut iterator = finite.iter_unset().zip(clone.iter_unset());
            for _ in 0..num_iters {
                let (expected, actual) = iterator.next().unwrap();
                assert_eq!(expected, actual);
            }
        }
        //
        assert_eq!(format!("{clone:?}"), display);
        assert_eq!(format!("{clone}"), display);
        assert_eq!(!clone, inverse);
    }

    #[quickcheck]
    fn arbitrary_extend(bitmap: Bitmap, extra: HashSet<BitmapIndex>) {
        let mut extended = bitmap.clone();
        extended.extend(extra.iter().copied());

        if let Some(bitmap_weight) = bitmap.weight() {
            let extra_weight = extended
                .weight()
                .unwrap()
                .checked_sub(bitmap_weight)
                .expect("Extending a bitmap shouldn't reduce the weight");
            assert!(extra_weight <= extra.len());
        }

        for idx in extra {
            assert!(extended.is_set(idx));
        }
    }

    #[quickcheck]
    fn arbitrary_op_index(bitmap: Bitmap, index: BitmapIndex) {
        test_indexing(&bitmap, index, bitmap.is_set(index))
    }

    #[quickcheck]
    fn arbitrary_op_range(bitmap: Bitmap, range: Range<BitmapIndex>) {
        let range_usize = usize::from(range.start)..usize::from(range.end);
        let range_len = range_usize.clone().count();

        let mut buf = bitmap.clone();
        buf.set_range(range.clone());
        if let Some(bitmap_weight) = bitmap.weight() {
            let extra_weight = buf
                .weight()
                .unwrap()
                .checked_sub(bitmap_weight)
                .expect("Setting indices shouldn't reduce the weight");
            assert!(extra_weight <= range_len);

            for idx in range_usize.clone() {
                assert!(buf.is_set(idx));
            }
        }

        buf.copy_from(&bitmap);
        buf.unset_range(range);
        if let Some(bitmap_weight) = bitmap.weight() {
            let lost_weight = bitmap_weight
                .checked_sub(buf.weight().unwrap())
                .expect("Clearing indices shouldn't increase the weight");
            assert!(lost_weight <= range_len);

            for idx in range_usize {
                assert!(!buf.is_set(idx));
            }
        }
    }

    #[quickcheck]
    fn arbitrary_op_bitmap(bitmap: Bitmap, other: Bitmap) {
        let (finite, infinite) = split_infinite_bitmap(bitmap.clone());
        let (other_finite, other_infinite) = split_infinite_bitmap(other.clone());

        assert_eq!(
            bitmap.includes(&other),
            other_finite.iter_set().all(|idx| bitmap.is_set(idx))
                && match (&infinite, &other_infinite) {
                    (Some(infinite), Some(other_infinite)) => {
                        (usize::from(other_infinite.start)..usize::from(infinite.start))
                            .all(|idx| finite.is_set(idx))
                    }
                    (_, None) => true,
                    (None, Some(_)) => false,
                }
        );

        fn infinite_intersects_finite(infinite: &RangeFrom<BitmapIndex>, finite: &Bitmap) -> bool {
            finite
                .last_set()
                .map(|last_set| infinite.start <= last_set)
                .unwrap_or(false)
        }
        assert_eq!(
            bitmap.intersects(&other),
            finite.iter_set().any(|idx| other.is_set(idx))
                || match (&infinite, &other_infinite) {
                    (Some(_), Some(_)) => true,
                    (Some(infinite), None) => infinite_intersects_finite(infinite, &other_finite),
                    (None, Some(other_infinite)) =>
                        infinite_intersects_finite(other_infinite, &finite),
                    (None, None) => false,
                }
        );

        assert_eq!(
            bitmap == other,
            bitmap.includes(&other) && other.includes(&bitmap)
        );

        fn expected_cmp(bitmap: &Bitmap, reference: &Bitmap) -> Ordering {
            let (finite, infinite) = split_infinite_bitmap(bitmap.clone());
            let (ref_finite, ref_infinite) = split_infinite_bitmap(reference.clone());

            let finite_end = match (infinite, ref_infinite) {
                (Some(_), None) => return Ordering::Greater,
                (None, Some(_)) => return Ordering::Less,
                (Some(infinite), Some(ref_infinite)) => infinite.start.max(ref_infinite.start),
                (None, None) => finite
                    .last_set()
                    .unwrap_or(BitmapIndex::MIN)
                    .max(ref_finite.last_set().unwrap_or(BitmapIndex::MIN)),
            };

            for idx in (0..=usize::from(finite_end)).rev() {
                match (bitmap.is_set(idx), reference.is_set(idx)) {
                    (true, false) => return Ordering::Greater,
                    (false, true) => return Ordering::Less,
                    _ => continue,
                }
            }
            Ordering::Equal
        }
        assert_eq!(bitmap.cmp(&other), expected_cmp(&bitmap, &other));

        let mut bitmap_and_other = finite
            .iter_set()
            .filter(|idx| other.is_set(*idx))
            .collect::<Bitmap>();
        match (&infinite, &other_infinite) {
            (Some(infinite), Some(other_infinite)) => {
                bitmap_and_other.set_range(infinite.start.max(other_infinite.start)..);
                for idx in usize::from(infinite.start)..usize::from(other_infinite.start) {
                    if other.is_set(idx) {
                        bitmap_and_other.set(idx);
                    }
                }
            }
            (Some(infinite), None) => {
                let other_end = other_finite.last_set().unwrap_or(BitmapIndex::MIN);
                for idx in usize::from(infinite.start)..=usize::from(other_end) {
                    if other.is_set(idx) {
                        bitmap_and_other.set(idx)
                    }
                }
            }
            _ => {}
        }
        test_and_sub(&bitmap, &other, &bitmap_and_other);

        let mut bitmap_or_other = finite;
        for idx in &other_finite {
            bitmap_or_other.set(idx);
        }
        if let Some(infinite) = infinite {
            bitmap_or_other.set_range(infinite);
        }
        if let Some(other_infinite) = other_infinite {
            bitmap_or_other.set_range(other_infinite);
        }
        assert_eq!(&bitmap | &other, bitmap_or_other);
        let mut buf = bitmap.clone();
        buf |= &other;
        assert_eq!(buf, bitmap_or_other);

        let bitmap_xor_other = bitmap_or_other - bitmap_and_other;
        assert_eq!(&bitmap ^ &other, bitmap_xor_other);
        buf.copy_from(&bitmap);
        buf ^= &other;
        assert_eq!(buf, bitmap_xor_other);
    }
}
