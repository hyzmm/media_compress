#[cfg(target_os = "android")]
use std::sync::OnceLock;

#[cfg(target_os = "android")]
static JAVA_VM_PTR: OnceLock<usize> = OnceLock::new();

#[cfg(target_os = "android")]
pub fn init_java_vm(vm: *mut jni::sys::JavaVM) {
    if !vm.is_null() {
        let _ = JAVA_VM_PTR.set(vm as usize);
    }
}

#[cfg(target_os = "android")]
pub fn java_vm_ptr() -> Option<*mut jni::sys::JavaVM> {
    JAVA_VM_PTR
        .get()
        .copied()
        .map(|ptr| ptr as *mut jni::sys::JavaVM)
}
