#!/bin/bash
# Test script for OP-ZisK components
# Tests L1, L2, and proof generation components separately

set -e

echo "=========================================="
echo "OP-ZisK Component Testing"
echo "=========================================="
echo ""

# Colors
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Test 1: RPC Health Check
echo -e "${YELLOW}[TEST 1] RPC Health Check${NC}"
echo "Testing RPC connectivity and health..."

if [ -f ".devnet-rpcs.env" ]; then
    ENV_FILE=".devnet-rpcs.env"
    echo "Using devnet RPCs from .devnet-rpcs.env"
elif [ -f ".base-sepolia.env" ]; then
    ENV_FILE=".base-sepolia.env"
    echo "Using public Sepolia RPCs from .base-sepolia.env"
else
    echo -e "${RED}ERROR: No .env file found${NC}"
    exit 1
fi

source "$ENV_FILE"

# Test L1 RPC
echo -n "  L1 RPC ($L1_RPC): "
L1_BLOCK=$(curl -s -X POST "$L1_RPC" -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' 2>/dev/null | \
    python3 -c "import sys, json; r=json.load(sys.stdin); print(int(r.get('result', '0x0'), 16))" 2>/dev/null || echo "0")
if [ "$L1_BLOCK" != "0" ]; then
    echo -e "${GREEN}✓ Connected (block $L1_BLOCK)${NC}"
else
    echo -e "${RED}✗ Failed${NC}"
fi

# Test L2 RPC
echo -n "  L2 RPC ($L2_RPC): "
L2_BLOCK=$(curl -s -X POST "$L2_RPC" -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' 2>/dev/null | \
    python3 -c "import sys, json; r=json.load(sys.stdin); print(int(r.get('result', '0x0'), 16))" 2>/dev/null || echo "0")
if [ "$L2_BLOCK" != "0" ]; then
    echo -e "${GREEN}✓ Connected (block $L2_BLOCK)${NC}"
else
    echo -e "${RED}✗ Failed${NC}"
fi

# Test L2 Node RPC
echo -n "  L2 Node RPC ($L2_NODE_RPC): "
L2_NODE_STATUS=$(curl -s -X POST "$L2_NODE_RPC" -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"optimism_syncStatus","params":[],"id":1}' 2>/dev/null | \
    python3 -c "import sys, json; r=json.load(sys.stdin); print('OK' if r.get('result') else 'ERROR')" 2>/dev/null || echo "ERROR")
if [ "$L2_NODE_STATUS" = "OK" ]; then
    echo -e "${GREEN}✓ Connected${NC}"
else
    echo -e "${YELLOW}⚠ May not support optimism_syncStatus${NC}"
fi

echo ""

# Test 2: Configuration System
echo -e "${YELLOW}[TEST 2] Configuration System${NC}"
echo "Testing RPCConfig discovery and health check..."

if cargo run --release --bin prove-range -- --env-file "$ENV_FILE" --start 1 --end 2 --save-input /tmp/test-witness.bin --safe-db-fallback 2>&1 | grep -q "RPC health check passed"; then
    echo -e "${GREEN}✓ Configuration system working${NC}"
else
    echo -e "${RED}✗ Configuration system failed${NC}"
    echo "  (This is expected if devnet is not running)"
fi

echo ""

# Test 3: Binary Availability
echo -e "${YELLOW}[TEST 3] Binary and Dependencies${NC}"

# Check prove-range binary
if [ -f "target/release/prove-range" ]; then
    echo -e "${GREEN}✓ prove-range binary exists${NC}"
else
    echo -e "${RED}✗ prove-range binary not found${NC}"
fi

# Check cargo-zisk
if command -v cargo-zisk &> /dev/null; then
    echo -e "${GREEN}✓ cargo-zisk available${NC}"
else
    echo -e "${RED}✗ cargo-zisk not found${NC}"
fi

# Check witness library
if [ -f "$HOME/.zisk/bin/libzisk_witness.dylib" ] || [ -f "$HOME/.zisk/bin/libzisk_witness.so" ]; then
    echo -e "${GREEN}✓ Witness library found${NC}"
else
    echo -e "${RED}✗ Witness library not found${NC}"
fi

# Check ROM setup
if [ -d "$HOME/.zisk/cache" ] && [ "$(ls -A $HOME/.zisk/cache 2>/dev/null)" ]; then
    echo -e "${GREEN}✓ ROM cache exists (setup may be complete)${NC}"
else
    echo -e "${YELLOW}⚠ ROM cache not found (ROM setup may be needed)${NC}"
fi

echo ""

# Test 4: Code Compilation
echo -e "${YELLOW}[TEST 4] Code Compilation${NC}"
if cargo check --package op-zisk-config 2>&1 | grep -q "Finished"; then
    echo -e "${GREEN}✓ Config package compiles${NC}"
else
    echo -e "${RED}✗ Config package has compilation errors${NC}"
fi

echo ""

# Summary
echo "=========================================="
echo "Test Summary"
echo "=========================================="
echo ""
echo "Next steps:"
echo "1. If devnet is not running, start it:"
echo "   cd tests && just test-e2e-sysgo ./e2e/validity/... TestValidityProposer_L2OODeployedAndUp"
echo ""
echo "2. For witness generation test (requires devnet or archive node):"
echo "   ./target/release/prove-range --env-file $ENV_FILE --start <recent_block> --end <recent_block+1> --save-input test-witness.bin"
echo ""
echo "3. For ROM setup (one-time, takes 5-60 minutes):"
echo "   cargo-zisk rom-setup -e target/riscv64ima-zisk-zkvm-elf/release/range -k \$HOME/.zisk/provingKey"
echo ""

