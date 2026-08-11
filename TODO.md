# Roadmap

## Archiving and compression

Add optional archive support for common workflows without making the core file
manager depend on a large collection of utilities.

Planned scope:

- Create and extract common archive formats from the file manager.
- Inspect archive contents before extraction.
- Show progress, support cancellation, and avoid overwriting existing files by
  default.
- Detect unavailable archive tools and explain which packages are required when
  the feature is opened.
- Protect against unsafe archive paths and preserve file metadata where the
  selected format supports it.

The feature should remain optional: browsing and ordinary file operations must
continue to work without archive utilities installed.
