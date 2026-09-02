// Package main builds rclone as a static C library for iOS.
//
// The four exported functions are rclone's OWN librclone API
// (github.com/rclone/rclone/librclone/librclone), re-exported here rather
// than reimplemented. Everything goes through one JSON-RPC entry point:
//
//	RcloneRPC("operations/list", `{"fs":"remote:","remote":"dir"}`)
//
// Why this file exists at all, given rclone ships librclone/gomobile:
// that package hardcodes `_ "github.com/rclone/rclone/backend/all"`, which
// links all 70 backends (~26 MB gz). Our entry point imports the trimmed set
// from ./backends instead. The RPC surface is identical — only the backend
// list differs.
//
// Upstream marks these interfaces "experimental and may change", and says
// "iOS has not been tested (but should probably work)". That is why this is
// pinned to an exact rclone version in go.mod rather than tracking a branch.
package main

/*
#include <stdlib.h>
*/
import "C"

import (
	"unsafe"

	_ "github.com/legsegue-gif/solos-rclone/backends"
	"github.com/rclone/rclone/librclone/librclone"
)

//export SolosRcloneInitialize
func SolosRcloneInitialize() {
	librclone.Initialize()
}

//export SolosRcloneFinalize
func SolosRcloneFinalize() {
	librclone.Finalize()
}

// SolosRcloneRPC runs one rclone RPC call.
//
// Returns a malloc'd C string the caller MUST release with
// SolosRcloneFreeString — Go's GC does not own it. `status` is an HTTP-style
// code: 200 = OK.
//
//export SolosRcloneRPC
func SolosRcloneRPC(method *C.char, input *C.char, status *C.int) *C.char {
	out, code := librclone.RPC(C.GoString(method), C.GoString(input))
	if status != nil {
		*status = C.int(code)
	}
	return C.CString(out)
}

//export SolosRcloneFreeString
func SolosRcloneFreeString(s *C.char) {
	C.free(unsafe.Pointer(s))
}

func main() {}
