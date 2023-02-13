//! Topology building

// - Creation and destruction: https://hwloc.readthedocs.io/en/v2.9/group__hwlocality__creation.html
// - Discovery source: https://hwloc.readthedocs.io/en/v2.9/group__hwlocality__setsource.html
// - Detection configuration and query: https://hwloc.readthedocs.io/en/v2.9/group__hwlocality__configuration.html

use crate::{ffi, ProcessId, RawTopology, Topology};
use bitflags::bitflags;
use errno::{errno, Errno};
use libc::{EINVAL, ENOSYS};
use std::{
    ffi::{c_ulong, CString},
    fmt::Debug,
    path::Path,
    ptr::NonNull,
};
use thiserror::Error;

/// Mechanism to build a `Topology` with custom configuration
pub struct TopologyBuilder(NonNull<RawTopology>);

impl TopologyBuilder {
    // === Topology building: https://hwloc.readthedocs.io/en/v2.9/group__hwlocality__creation.html ===

    /// Start building a `Topology`
    pub fn new() -> Self {
        let mut topology: *mut RawTopology = std::ptr::null_mut();
        let result = unsafe { ffi::hwloc_topology_init(&mut topology) };
        assert_ne!(result, -1, "Failed to allocate topology");
        assert_eq!(result, 0, "Unexpected hwloc_topology_init result {result}");
        Self(NonNull::new(topology).expect("Got null pointer from hwloc_topology_init"))
    }

    /// Load the topology with the previously specified parameters
    ///
    /// hwloc does not specify how this function can error out, but it usually
    /// sets Errno, hopefully you will find its value insightful...
    pub fn build(mut self) -> Result<Topology, Errno> {
        // Finalize the topology building
        let result = unsafe { ffi::hwloc_topology_load(self.as_mut_ptr()) };
        assert!(
            result == 0 || result == -1,
            "Unexpected hwloc_topology_load result {result} with errno {}",
            errno()
        );

        // If that was successful, transfer RawTopology ownership to a Topology
        if result == 0 {
            let result = Topology(self.0);
            std::mem::forget(self);
            Ok(result)
        } else {
            Err(errno())
        }
    }

    // === Discovery source: https://hwloc.readthedocs.io/en/v2.9/group__hwlocality__setsource.html ===

    /// Change which process the topology is viewed from
    ///
    /// On some systems, processes may have different views of the machine, for
    /// instance the set of allowed CPUs. By default, hwloc exposes the view
    /// from the current process. Calling this method permits to make it expose
    /// the topology of the machine from the point of view of another process.
    pub fn from_pid(mut self, pid: ProcessId) -> Result<Self, Unsupported> {
        let result = unsafe { ffi::hwloc_topology_set_pid(self.as_mut_ptr(), pid) };
        match result {
            0 => Ok(self),
            -1 => {
                let errno = errno();
                match errno.0 {
                    ENOSYS => Err(Unsupported(self)),
                    _ => panic!("Unexpected errno {errno}"),
                }
            }
            other => panic!("Unexpected result {other} with errno {}", errno()),
        }
    }

    /// Read the topology from a synthetic textual description
    ///
    /// Instead of being probed from the host system, topology information will
    /// be read from the given
    /// [textual description](https://hwloc.readthedocs.io/en/v2.9/synthetic.html).
    ///
    /// Setting the environment variable `HWLOC_SYNTHETIC` may also result in
    /// this behavior.
    ///
    /// CPU and memory binding operations will be ineffective with this backend.
    pub fn from_synthetic(mut self, description: &str) -> Result<Self, InvalidParameter> {
        let Ok(description) = CString::new(description) else { return Err(InvalidParameter(self)) };
        let result =
            unsafe { ffi::hwloc_topology_set_synthetic(self.as_mut_ptr(), description.as_ptr()) };
        match result {
            0 => Ok(self),
            -1 => {
                let errno = errno();
                match errno.0 {
                    EINVAL => Err(InvalidParameter(self)),
                    _ => panic!("Unexpected errno {errno}"),
                }
            }
            other => panic!("Unexpected result {other} with errno {}", errno()),
        }
    }

    /// Read the topology from an XML description
    ///
    /// Instead of being probed from the host system, topology information will
    /// be read from the given
    /// [XML description](https://hwloc.readthedocs.io/en/v2.9/xml.html).
    ///
    /// CPU and memory binding operations will be ineffective with this backend,
    /// unless `BuildFlags::ASSUME_THIS_SYSTEM` is set to assert that the loaded
    /// XML file truly matches the underlying system.
    pub fn from_xml(mut self, xml: &str) -> Result<Self, InvalidParameter> {
        let Ok(xml) = CString::new(xml) else { return Err(InvalidParameter(self)) };
        let result = unsafe {
            ffi::hwloc_topology_set_xmlbuffer(
                self.as_mut_ptr(),
                xml.as_ptr(),
                xml.as_bytes()
                    .len()
                    .try_into()
                    .expect("XML buffer is too big for hwloc"),
            )
        };
        match result {
            0 => Ok(self),
            -1 => {
                let errno = errno();
                match errno.0 {
                    EINVAL => Err(InvalidParameter(self)),
                    _ => panic!("Unexpected errno {errno}"),
                }
            }
            other => panic!("Unexpected result {other} with errno {}", errno()),
        }
    }

    /// Read the topology from an XML file
    ///
    /// This works a lot like `from_xml()`, but takes a file name as a parameter
    /// instead of an XML string. The same effect can be achieved by setting the
    /// `HWLOC_XMLFILE` environment variable.
    pub fn from_xml_file(mut self, path: impl AsRef<Path>) -> Result<Self, InvalidParameter> {
        let Some(path) = path.as_ref().to_str() else { return Err(InvalidParameter(self)) };
        let Ok(path) = CString::new(path) else { return Err(InvalidParameter(self)) };
        let result = unsafe { ffi::hwloc_topology_set_xml(self.as_mut_ptr(), path.as_ptr()) };
        match result {
            0 => Ok(self),
            -1 => {
                let errno = errno();
                match errno.0 {
                    EINVAL => Err(InvalidParameter(self)),
                    _ => panic!("Unexpected errno {errno}"),
                }
            }
            other => panic!("Unexpected result {other} with errno {}", errno()),
        }
    }

    /// Prevent a discovery component from being used for a topology
    ///
    /// `name` is the name of the discovery component that should not be used
    /// when loading topology topology. The name is a string such as "cuda".
    /// For components with multiple phases, it may also be suffixed with the
    /// name of a phase, for instance "linux:io". A list of components
    /// distributed with hwloc can be found
    /// [in the hwloc documentation](https://hwloc.readthedocs.io/en/v2.9/plugins.html#plugins_list).
    ///
    /// This may be used to avoid expensive parts of the discovery process. For
    /// instance, CUDA-specific discovery may be expensive and unneeded while
    /// generic I/O discovery could still be useful.
    pub fn blacklist_component(mut self, name: &str) -> Result<Self, InvalidParameter> {
        let Ok(name) = CString::new(name) else { return Err(InvalidParameter(self)) };
        let result = unsafe {
            ffi::hwloc_topology_set_components(
                self.as_mut_ptr(),
                ComponentsFlags::BLACKLIST,
                name.as_ptr(),
            )
        };
        assert!(
            result >= 0,
            "Unexpected result {result} with errno {}",
            errno()
        );
        Ok(self)
    }

    // === Detection config/query: https://hwloc.readthedocs.io/en/v2.9/group__hwlocality__configuration.html ===

    /// Set topology building flags
    ///
    /// If this function is called multiple times, the last invocation will
    /// erase and replace the set of flags that was previously set.
    ///
    /// # Examples
    ///
    /// ```
    /// use hwloc2::{Topology, builder::BuildFlags};
    ///
    /// let topology = Topology::builder()
    ///                         .with_flags(BuildFlags::ASSUME_THIS_SYSTEM).unwrap()
    ///                         .build().unwrap();
    /// ```
    ///
    pub fn with_flags(mut self, flags: BuildFlags) -> Result<Self, InvalidParameter> {
        let result = unsafe { ffi::hwloc_topology_set_flags(self.as_mut_ptr(), flags.bits()) };
        match result {
            0 => Ok(self),
            -1 => {
                let errno = errno();
                match errno.0 {
                    EINVAL => Err(InvalidParameter(self)),
                    _ => panic!("Unexpected errno {errno}"),
                }
            }
            other => panic!("Unexpected result {other} with errno {}", errno()),
        }
    }

    /// Check current topology building flags
    pub fn flags(&self) -> BuildFlags {
        BuildFlags::from_bits(unsafe { ffi::hwloc_topology_get_flags(self.as_ptr()) })
            .expect("Encountered unexpected topology flags")
    }

    // === General-purpose internal utilities ===

    /// Returns the contained hwloc topology pointer for interaction with hwloc.
    fn as_ptr(&self) -> *const RawTopology {
        self.0.as_ptr() as *const RawTopology
    }

    /// Returns the contained hwloc topology pointer for interaction with hwloc.
    fn as_mut_ptr(&mut self) -> *mut RawTopology {
        self.0.as_ptr()
    }
}

impl Debug for TopologyBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TopologyBuilder")
    }
}

impl Drop for TopologyBuilder {
    fn drop(&mut self) {
        unsafe { ffi::hwloc_topology_destroy(self.as_mut_ptr()) }
    }
}

bitflags! {
    /// Topology building configuration flags
    #[repr(C)]
    pub struct BuildFlags: c_ulong {
        /// Detect the whole system, ignore reservations, include disallowed objects
        ///
        /// Gather all online resources, even if some were disabled by the
        /// administrator. For instance, ignore Linux Cgroup/Cpusets and gather
        /// all processors and memory nodes. However offline PUs and NUMA nodes
        /// are still ignored.
        ///
        /// When this flag is not set, PUs and NUMA nodes that are disallowed
        /// are not added to the topology. Parent objects (package, core, cache,
        /// etc.) are added only if some of their children are allowed. All
        /// existing PUs and NUMA nodes in the topology are allowed.
        /// `Topology::allowed_cpuset()` and `Topology::allowed_nodeset()` are
        /// equal to the root object cpuset and nodeset.
        ///
        /// When this flag is set, the actual sets of allowed PUs and NUMA nodes
        /// are given by `Topology::allowed_cpuset()` and
        /// `Topology::allowed_nodeset()`. They may be smaller than the root
        /// object cpuset and nodeset.
        ///
        /// If the current topology is exported to XML and reimported later,
        /// this flag should be set again in the reimported topology so that
        /// disallowed resources are reimported as well.
        const INCLUDE_DISALLOWED = (1<<0);

        /// Assume that the selected backend provides the topology for the
        /// system on which we are running
        ///
        /// This forces `Topology::is_this_system()` to return true, i.e. makes
        /// hwloc assume that the selected backend provides the topology for the
        /// system on which we are running, even if it is not the OS-specific
        /// backend but the XML backend for instance. This means making the
        /// binding functions actually call the OS-specific system calls and
        /// really do binding, while the XML backend would otherwise provide
        /// empty hooks just returning success.
        ///
        /// Setting the environment variable HWLOC_THISSYSTEM may also result in
        /// the same behavior.
        ///
        /// This can be used for efficiency reasons to first detect the topology
        /// once, save it to an XML file, and quickly reload it later through
        /// the XML backend, but still having binding functions actually do bind.
        const ASSUME_THIS_SYSTEM = (1<<1); // aka HWLOC_TOPOLOGY_FLAG_IS_THISSYSTEM

        /// Get the set of allowed resources from the local operating system
        /// even if the topology was loaded from XML or synthetic description
        ///
        /// If the topology was loaded from XML or from a synthetic string,
        /// restrict it by applying the current process restrictions such as
        /// Linux Cgroup/Cpuset.
        ///
        /// This is useful when the topology is not loaded directly from the
        /// local machine (e.g. for performance reason) and it comes with all
        /// resources, while the running process is restricted to only parts of
        /// the machine.
        ///
        /// This flag is ignored unless `ASSUME_THIS_SYSTEM` is also set since
        /// the loaded topology must match the underlying machine where
        /// restrictions will be gathered from.
        ///
        /// Setting the environment variable HWLOC_THISSYSTEM_ALLOWED_RESOURCES
        /// would result in the same behavior.
        const GET_ALLOWED_RESOURCES_FROM_THIS_SYSTEM = (1<<2); // aka HWLOC_TOPOLOGY_FLAG_THISSYSTEM_ALLOWED_RESOURCES

        /// Import support from the imported topology
        ///
        /// When importing a XML topology from a remote machine, binding is
        /// disabled by default (see `ASSUME_THIS_SYSTEM`). This disabling is
        /// also marked by putting zeroes in the corresponding supported feature
        /// bits reported by `Topology::support()`.
        ///
        /// The flag `IMPORT_SUPPORT` allows you to actually import support bits
        /// from the remote machine. It also sets the `MiscSupport::imported()`
        /// support flag. If the imported XML did not contain any support
        /// information (exporter hwloc is too old), this flag is not set.
        ///
        /// Note that these supported features are only relevant for the hwloc
        /// installation that actually exported the XML topology (it may vary
        /// with the operating system, or with how hwloc was compiled).
        ///
        /// Note that setting this flag however does not enable binding for the
        /// locally imported hwloc topology, it only reports what the remote
        /// hwloc and machine support.
        const IMPORT_SUPPORT = (1<<3);

        /// Do not consider resources outside of the process CPU binding
        ///
        /// If the binding of the process is limited to a subset of cores,
        /// ignore the other cores during discovery.
        ///
        /// The resulting topology is identical to what a call to
        /// hwloc_topology_restrict() (TODO: adapt to binding) would generate,
        /// but this flag also prevents hwloc from ever touching other resources
        /// during the discovery.
        ///
        /// This flag especially tells the x86 backend to never temporarily
        /// rebind a thread on any excluded core. This is useful on Windows
        /// because such temporary rebinding can change the process binding.
        /// Another use-case is to avoid cores that would not be able to perform
        /// the hwloc discovery anytime soon because they are busy executing
        /// some high-priority real-time tasks.
        ///
        /// If process CPU binding is not supported, the thread CPU binding is
        /// considered instead if supported, or the flag is ignored.
        ///
        /// This flag requires `ASSUME_THIS_SYSTEM as well since binding support
        /// is required.
        const RESTRICT_CPU_TO_THIS_PROCESS = (1<<4); // aka HWLOC_TOPOLOGY_FLAG_RESTRICT_TO_CPUBINDING

        /// Do not consider resources outside of the process memory binding
        ///
        /// If the binding of the process is limited to a subset of NUMA nodes,
        /// ignore the other NUMA nodes during discovery.
        ///
        /// The resulting topology is identical to what a call to
        /// hwloc_topology_restrict() (TODO: adapt to binding) would generate,
        /// but this flag also prevents hwloc from ever touching other resources
        /// during the discovery.
        ///
        /// This flag is meant to be used together with
        /// `RESTRICT_CPU_TO_THIS_PROCESS` when both cores and NUMA nodes should
        /// be ignored outside of the process binding.
        ///
        /// If process memory binding is not supported, the thread memory
        /// binding is considered instead if supported, or the flag is ignored.
        ///
        /// This flag requires `ASSUME_THIS_SYSTEM` as well since binding
        /// support is required.
        const RESTRICT_MEMORY_TO_THIS_PROCESS = (1<<5); // aka HWLOC_TOPOLOGY_FLAG_RESTRICT_TO_MEMBINDING

        /// Do not ever modify the process or thread binding during discovery
        ///
        /// This flag disables all hwloc discovery steps that require a change
        /// of the process or thread binding. This currently only affects the
        /// x86 backend which gets entirely disabled.
        ///
        /// This is useful when a `Topology` is loaded while the application
        /// also creates additional threads or modifies the binding.
        ///
        /// This flag is also a strict way to make sure the process binding will
        /// not change to due thread binding changes on Windows (see
        /// `RESTRICT_CPU_TO_THIS_PROCESS`).
        const DONT_CHANGE_BINDING = (1<<6);

        /// Ignore distances
        ///
        /// Ignore distance information from the operating systems (and from
        /// XML) and hence do not use distances for grouping.
        const IGNORE_DISTANCES = (1<<7); // aka HWLOC_TOPOLOGY_FLAG_NO_DISTANCES

        /// Ignore memory attributes
        ///
        /// Ignore memory attribues from the operating systems (and from XML).
        const IGNORE_MEMORY_ATTRIBUTES = (1<<8); // aka HWLOC_TOPOLOGY_FLAG_NO_MEMATTRS

        /// Ignore CPU Kinds
        ///
        /// Ignore CPU kind information from the operating systems (and from
        /// XML).
        const IGNORE_CPU_KINDS = (1<<9); // aka HWLOC_TOPOLOGY_FLAG_NO_CPUKINDS
    }
}

impl Default for BuildFlags {
    fn default() -> Self {
        Self::empty()
    }
}

bitflags! {
    /// Flags to be passed to `hwloc_topology_set_components()`
    #[repr(C)]
    pub(crate) struct ComponentsFlags: c_ulong {
        /// Blacklist the target component from being used
        const BLACKLIST = (1<<0);
    }
}

/// Error returned when an invalid parameter was passed
#[derive(Debug, Error)]
#[error("invalid parameter specified")]
pub struct InvalidParameter(TopologyBuilder);

/// Error returned when the platform does not support this feature
#[derive(Debug, Error)]
#[error("platform does not support this feature")]
pub struct Unsupported(TopologyBuilder);
