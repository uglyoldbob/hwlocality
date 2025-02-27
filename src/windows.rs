//! Windows-specific helpers

use crate::{
    cpu::cpusets::CpuSet,
    errors::{self, RawHwlocError},
    ffi,
    topology::Topology,
};
use std::{ffi::c_uint, iter::FusedIterator, num::NonZeroUsize};

/// # Windows-specific helpers
///
/// These functions query Windows processor groups. These groups partition the
/// operating system into virtual sets of up to 64 neighbor PUs. Threads and
/// processes may only be bound inside a single group. Although Windows
/// processor groups may be exposed in the hwloc hierarchy as hwloc Groups,
/// they are also often merged into existing hwloc objects such as NUMA nodes
/// or Packages. This API provides explicit information about Windows processor
/// groups so that applications know whether binding to a large set of PUs may
/// fail because it spans over multiple Windows processor groups.
//
// Upstream docs: https://hwloc.readthedocs.io/en/v2.9/group__hwlocality__windows.html
impl Topology {
    /// Number of Windows processor groups
    ///
    /// # Errors
    ///
    /// One reason why this function can fail is if the topology does not match
    /// the current system (e.g. loaded from another machine through XML).
    #[doc(alias = "hwloc_windows_get_nr_processor_groups")]
    pub fn num_processor_groups(&self) -> Result<NonZeroUsize, RawHwlocError> {
        let count =
            errors::call_hwloc_int_normal("hwloc_windows_get_nr_processor_groups", || unsafe {
                ffi::hwloc_windows_get_nr_processor_groups(self.as_ptr(), 0)
            })?;
        let count = NonZeroUsize::new(ffi::expect_usize(count))
            .expect("Unexpected 0 processor group count");
        Ok(count)
    }

    /// Enumerate the cpusets of Windows processor groups
    ///
    /// # Errors
    ///
    /// One reason why this function can fail is if the topology does not match
    /// the current system (e.g. loaded from another machine through XML).
    #[doc(alias = "hwloc_windows_get_processor_group_cpuset")]
    pub fn processor_groups(
        &self,
    ) -> Result<
        impl Iterator<Item = Result<CpuSet, RawHwlocError>>
            + Clone
            + DoubleEndedIterator
            + ExactSizeIterator
            + FusedIterator
            + '_,
        RawHwlocError,
    > {
        Ok(
            (0..usize::from(self.num_processor_groups()?)).map(|pg_index| {
                let mut set = CpuSet::new();
                let pg_index = c_uint::try_from(pg_index)
                    .expect("Can't fail, pg_index upper bound comes from hwloc");
                errors::call_hwloc_int_normal(
                    "hwloc_windows_get_processor_group_cpuset",
                    || unsafe {
                        ffi::hwloc_windows_get_processor_group_cpuset(
                            self.as_ptr(),
                            pg_index,
                            set.as_mut_ptr(),
                            0,
                        )
                    },
                )?;
                Ok(set)
            }),
        )
    }
}
