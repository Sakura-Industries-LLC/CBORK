#!/usr/bin/env bash
set -euo pipefail

KEYSET_SCHEMA="test/dntls-core/doc/tests/dntls-keyset-vectors.cddl"

validate_keyset_vectors() {
  vector_dir="$1"

  for size in 1 2 3 4 5; do
    cbork validate --type=general-keyset "$KEYSET_SCHEMA" "$vector_dir/general_keyset_$size.cbor"
    cbork validate --type=rotatable-keyset "$KEYSET_SCHEMA" "$vector_dir/rotatable_keyset_$size.cbor"
  done

  for size in 1 3 5; do
    cbork validate --type=rotatable-odd-keyset "$KEYSET_SCHEMA" "$vector_dir/rotatable_keyset_$size.cbor"
  done

  for size in 2 4; do
    cbork validate --fails --type=rotatable-odd-keyset "$KEYSET_SCHEMA" "$vector_dir/rotatable_keyset_$size.cbor"
  done
}

cd /repo

if [[ ! -d test ]]; then
    echo "error: test/ does not exist" >&2
    exit 1
fi

if ! find test -type f -name '*.cddl' -print -quit | grep -q .; then
    echo "error: no .cddl files found under test/" >&2
    exit 1
fi

cargo build --release -p cbork

export PATH="/repo/target/release:$PATH"

cbork lint test/ --stats --summary --why --strict --doc

cbork render test/svcrec/doc/service-record-v1.cddl

#cbork validate --detailed --type=Null-Headers test/dntls-core/doc/tests/dntls-cose-defs-vectors.cddl "test/dntls-core/vectors/null_headers.cbor"
#cbork validate --detailed --type=Null-Headers test/dntls-core/doc/tests/dntls-cose-defs-vectors.cddl "test/dntls-core/vectors/null_headers-wrong-array.cbor"
#validate_keyset_vectors test/dntls-core/vectors
#cargo run -p cbork -- lint test/pqsig/doc/pqsig.cddl --stats --summary --why --strict
#cargo run -p cbork -- render test/pqsig/doc/pqsig.cddl
#cargo run -p cbork -- decode test/pqsig/vectors/4k/private_ed25519_mldsa44.cbor --pretty
# cargo run -p cbork -- validate test/pqsig/doc/pqsig.cddl test/pqsig/vectors/4k/private_ed25519_mldsa44.cbor --detailed

# cargo run -p cbork -- lint test/name-reg-tx/doc/name-reg-tx-v1.cddl --stats --summary --why

#cargo run -p cbork -- lint test/svcrec/doc/svcrec.cddl
