#!/bin/sh
set -eu
# shellcheck source=/dev/null
. /opt/ethereum-prover/artifacts/recursion.env
exec /opt/ethereum-prover/ethereum_prover/ethereum_prover "$@"
