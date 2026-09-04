;; tap-probe-cell — WAMR-ABI fixture for ABI conformance tests (SR-10b)
;;
;; This WAT module imports the six tap host functions from the "tap" module
;; namespace with the WAMR-identical signatures (all params/returns are i32).
;; It mirrors exactly what an ESP32-compiled cell binary does at the WASM ABI
;; boundary; the signatures match wasm-runtime/src/imports/tap.rs:
;;   tap_resolve        (*~)i    → (i32 i32) → i32
;;   tap_read_retained  (i*~*~)i → (i32 i32 i32 i32 i32) → i32
;;   tap_take_event     (i*~)i   → (i32 i32 i32) → i32
;;   tap_drain_batch    (i*~)i   → (i32 i32 i32) → i32
;;   tap_list_len       ()i      → () → i32
;;   tap_list_entry     (i*~*~)i → (i32 i32 i32 i32 i32) → i32
;;
;; Build command (from this file's directory):
;;   wasm-tools parse tap_probe_cell.wat -o tap_probe_wamr_abi.wasm
;;
;; The resulting binary is committed as `tap_probe_wamr_abi.wasm` and loaded
;; byte-identical by `tests/e2e.rs::abi_conformance_*` tests.

(module
  ;; ── imports (WAMR-identical ABI, all i32) ─────────────────────────────────
  (import "tap" "tap_resolve"
    (func $tap_resolve (param i32 i32) (result i32)))
  (import "tap" "tap_read_retained"
    (func $tap_read_retained (param i32 i32 i32 i32 i32) (result i32)))
  (import "tap" "tap_take_event"
    (func $tap_take_event (param i32 i32 i32) (result i32)))
  (import "tap" "tap_drain_batch"
    (func $tap_drain_batch (param i32 i32 i32) (result i32)))
  (import "tap" "tap_list_len"
    (func $tap_list_len (result i32)))
  (import "tap" "tap_list_entry"
    (func $tap_list_entry (param i32 i32 i32 i32 i32) (result i32)))

  ;; shared memory — one 64 KiB page
  (memory (export "memory") 1)

  ;; ── exported wrapper functions (mirrors the cell SDK surface) ─────────────
  (func (export "tap_resolve") (param i32 i32) (result i32)
    local.get 0  local.get 1  call $tap_resolve)
  (func (export "tap_read_retained") (param i32 i32 i32 i32 i32) (result i32)
    local.get 0  local.get 1  local.get 2  local.get 3  local.get 4
    call $tap_read_retained)
  (func (export "tap_take_event") (param i32 i32 i32) (result i32)
    local.get 0  local.get 1  local.get 2  call $tap_take_event)
  (func (export "tap_drain_batch") (param i32 i32 i32) (result i32)
    local.get 0  local.get 1  local.get 2  call $tap_drain_batch)
  (func (export "tap_list_len") (result i32)
    call $tap_list_len)
  (func (export "tap_list_entry") (param i32 i32 i32 i32 i32) (result i32)
    local.get 0  local.get 1  local.get 2  local.get 3  local.get 4
    call $tap_list_entry)
)
