#!/usr/bin/env bash
# Shared Vulkan GPU env for Wordkeep/Ereshkigal llama.cpp.
# Source from mcp.sh / wiki.sh / rebuild_wordkeep.sh (not executed).
# CPU-only: WORDKEEP_FEATURES=embeddings,daslang,dashboard,ereshkigal

wordkeep_default_features() {
  printf '%s\n' "${WORDKEEP_FEATURES:-embeddings,daslang,dashboard,ereshkigal-vulkan}"
}

# llama.cpp Vulkan will otherwise bind lavapipe (software) on this host.
wordkeep_nvidia_icd() {
  local icd="/usr/share/vulkan/icd.d/nvidia_icd.json"
  if [[ -f "$icd" ]]; then
    export VK_ICD_FILENAMES="${VK_ICD_FILENAMES:-$icd}"
    export VK_DRIVER_FILES="${VK_DRIVER_FILES:-$icd}"
  fi
}

_wordkeep_find_vulkan_headers() {
  local cand
  for cand in \
    "${ERESHKIGAL_VULKAN_HEADERS:-}" \
    "${WK:-}/../ereshkigal/vendor/vulkan-headers" \
    "${WK:-}/ereshkigal/vendor/vulkan-headers" \
    "${WK:-}/../../../../ereshkigal/vendor/vulkan-headers" \
    "${HOME:-}/Documents/_Projects/ereshkigal/vendor/vulkan-headers" \
    /usr
  do
    [[ -n "$cand" ]] || continue
    if [[ -f "$cand/include/vulkan/vulkan.h" ]]; then
      printf '%s\n' "$cand"
      return 0
    fi
  done
  if [[ -f /usr/include/vulkan/vulkan.h ]]; then
    printf '%s\n' /usr
    return 0
  fi
  return 1
}

_wordkeep_find_spirv_prefix() {
  local cand
  for cand in \
    "${ERESHKIGAL_SPIRV_PREFIX:-}" \
    "${WK:-}/../ereshkigal/vendor/spirv-prefix" \
    "${WK:-}/ereshkigal/vendor/spirv-prefix" \
    "${WK:-}/../../../../ereshkigal/vendor/spirv-prefix" \
    "${HOME:-}/Documents/_Projects/ereshkigal/vendor/spirv-prefix"
  do
    [[ -n "$cand" ]] || continue
    if [[ -f "$cand/include/spirv/unified1/spirv.hpp" ]]; then
      printf '%s\n' "$cand"
      return 0
    fi
  done
  return 1
}

# CMAKE_ARGS for llama-cpp-sys-2 --features vulkan. No-op for CPU-only feature sets.
wordkeep_vulkan_cmake_env() {
  case ",$(wordkeep_default_features)," in
    *,ereshkigal-vulkan,*) ;;
    *) return 0 ;;
  esac
  local hdr spirv lib
  hdr="$(_wordkeep_find_vulkan_headers)" || {
    echo "[wordkeep:gpu] vulkan.h not found (install vulkan-headers or clone Khronos headers next to tools/ereshkigal/vendor)" >&2
    return 1
  }
  spirv="$(_wordkeep_find_spirv_prefix)" || spirv=""
  lib=""
  for lib in /usr/lib64/libvulkan.so /usr/lib/libvulkan.so; do
    [[ -e "$lib" ]] && break
    lib=""
  done
  [[ -n "$lib" ]] || {
    echo "[wordkeep:gpu] libvulkan.so not found" >&2
    return 1
  }
  export VULKAN_SDK="$hdr"
  export CMAKE_PREFIX_PATH="${hdr}${spirv:+:${spirv}}${CMAKE_PREFIX_PATH:+:${CMAKE_PREFIX_PATH}}"
  if [[ -n "$spirv" ]]; then
    export CPATH="${spirv}/include:${hdr}/include${CPATH:+:$CPATH}"
    export CPLUS_INCLUDE_PATH="${spirv}/include:${hdr}/include${CPLUS_INCLUDE_PATH:+:$CPLUS_INCLUDE_PATH}"
    export CMAKE_ARGS="-DVulkan_INCLUDE_DIR=${hdr}/include -DVulkan_LIBRARY=${lib} -DCMAKE_CXX_FLAGS=-I${spirv}/include -DCMAKE_C_FLAGS=-I${spirv}/include"
  else
    export CMAKE_ARGS="-DVulkan_INCLUDE_DIR=${hdr}/include -DVulkan_LIBRARY=${lib}"
  fi
  echo "[wordkeep:gpu] Vulkan cmake headers=$hdr lib=$lib${spirv:+ spirv=$spirv}" >&2
}

# CPU llama-cpp-sys-2 cmake trees have no Makefile once vulkan is flipped on.
wordkeep_wipe_cpu_llama_sys() {
  local target="${CARGO_TARGET_DIR:-${WK:-.}/target}"
  local cache dir build
  [[ -d "$target" ]] || return 0
  while IFS= read -r cache; do
    [[ -f "$cache" ]] || continue
    build="$(dirname "$cache")"
    dir="$(cd "$build/../.." && pwd)"
    if grep -q '^GGML_VULKAN:BOOL=OFF' "$cache" 2>/dev/null || [[ ! -f "$build/Makefile" && ! -f "$build/build.ninja" ]]; then
      echo "[wordkeep:gpu] removing stale llama-cpp-sys-2 tree $dir" >&2
      rm -rf "$dir"
    fi
  done < <(find "$target" -path '*llama-cpp-sys-2-*/out/build/CMakeCache.txt' -print 2>/dev/null || true)
}

wordkeep_bin_missing_vulkan() {
  local bin="$1"
  [[ -x "$bin" ]] || return 0
  if grep -a -q 'ggml-vulkan' "$bin" 2>/dev/null; then
    return 1
  fi
  return 0
}
