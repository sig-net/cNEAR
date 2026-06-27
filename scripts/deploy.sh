#!/usr/bin/env bash
set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Store original arg count for interactive mode detection
ORIGINAL_ARGC=$#

# Parse command line arguments
NETWORK=""
SIGNER_ID=""
DRY_RUN=false

while [[ $# -gt 0 ]]; do
    case $1 in
        testnet|mainnet)
            NETWORK="$1"
            shift
            ;;
        --dry-run)
            DRY_RUN=true
            shift
            ;;
        --help|-h)
            echo "Usage: $0 [testnet|mainnet] [signer_id] [--dry-run]"
            echo ""
            echo "Options:"
            echo "  testnet|mainnet    Target network (default: testnet)"
            echo "  signer_id          Account ID to use for deployment"
            echo "  --dry-run          Show commands without executing"
            echo ""
            echo "Examples:"
            echo "  $0                              # Interactive mode"
            echo "  $0 testnet                      # Interactive signer selection"
            echo "  $0 testnet alice.testnet        # Full CLI mode"
            echo "  $0 mainnet bob.near --dry-run   # Dry run"
            exit 0
            ;;
        *)
            if [[ -z "$SIGNER_ID" ]]; then
                SIGNER_ID="$1"
            else
                echo -e "${RED}Error: Unknown argument '$1'${NC}"
                exit 1
            fi
            shift
            ;;
    esac
done

# Interactive network selection if not provided
if [[ -z "$NETWORK" ]]; then
    echo -e "${BLUE}Select network:${NC}"
    echo "  1) testnet (default)"
    echo "  2) mainnet"
    read -p "Enter choice [1]: " network_choice
    network_choice=${network_choice:-1}
    
    case $network_choice in
        1) NETWORK="testnet" ;;
        2) NETWORK="mainnet" ;;
        *) echo -e "${RED}Invalid choice. Using testnet.${NC}"; NETWORK="testnet" ;;
    esac
fi

echo -e "${GREEN}Network: $NETWORK${NC}"

# Determine credentials directory
if [[ -n "$NEAR_CREDENTIALS" ]]; then
    CREDS_DIR="$NEAR_CREDENTIALS/$NETWORK"
elif [[ -d "$HOME/.near-credentials/$NETWORK" ]]; then
    CREDS_DIR="$HOME/.near-credentials/$NETWORK"
else
    echo -e "${RED}Error: NEAR credentials directory not found${NC}"
    echo "Expected: ~/.near-credentials/$NETWORK or \$NEAR_CREDENTIALS/$NETWORK"
    exit 1
fi

if [[ ! -d "$CREDS_DIR" ]]; then
    echo -e "${RED}Error: Credentials directory does not exist: $CREDS_DIR${NC}"
    exit 1
fi

# Interactive signer selection if not provided
if [[ -z "$SIGNER_ID" ]]; then
    echo -e "\n${BLUE}Available accounts in $CREDS_DIR:${NC}"
    
    # List all .json files and extract account IDs
    ACCOUNTS=()
    i=1
    while IFS= read -r file; do
        account_id=$(basename "$file" .json)
        ACCOUNTS+=("$account_id")
        echo "  $i) $account_id"
        ((i++))
    done < <(find "$CREDS_DIR" -maxdepth 1 -name "*.json" -type f | sort)
    
    if [[ ${#ACCOUNTS[@]} -eq 0 ]]; then
        echo -e "${RED}Error: No account credentials found in $CREDS_DIR${NC}"
        exit 1
    fi
    
    read -p "Select account [1]: " account_choice
    account_choice=${account_choice:-1}
    
    if [[ $account_choice -lt 1 || $account_choice -gt ${#ACCOUNTS[@]} ]]; then
        echo -e "${RED}Invalid choice${NC}"
        exit 1
    fi
    
    SIGNER_ID="${ACCOUNTS[$((account_choice-1))]}"
fi

echo -e "${GREEN}Signer: $SIGNER_ID${NC}"

# Verify signer credentials exist
SIGNER_KEY_FILE="$CREDS_DIR/${SIGNER_ID}.json"
if [[ ! -f "$SIGNER_KEY_FILE" ]]; then
    if [[ "$DRY_RUN" == "false" ]]; then
        echo -e "${RED}Error: Credentials file not found: $SIGNER_KEY_FILE${NC}"
        exit 1
    else
        echo -e "${YELLOW}Warning: Credentials file not found: $SIGNER_KEY_FILE (continuing in dry-run)${NC}"
    fi
else
    echo -e "${GREEN}✓ Credentials found${NC}"
fi

# Check if wasms exist (warn but continue in dry-run)
TOKEN_WASM="target/near/fungible_token.wasm"
CONTROLLER_WASM="target/near/aurora-controller-factory.wasm"

if [[ ! -f "$TOKEN_WASM" ]]; then
    if [[ "$DRY_RUN" == "false" ]]; then
        echo -e "${RED}Error: Token wasm not found at $TOKEN_WASM${NC}"
        echo "Run: just build-token"
        exit 1
    else
        echo -e "${YELLOW}Warning: Token wasm not found at $TOKEN_WASM (continuing in dry-run)${NC}"
    fi
fi

if [[ ! -f "$CONTROLLER_WASM" ]]; then
    if [[ "$DRY_RUN" == "false" ]]; then
        echo -e "${RED}Error: Controller wasm not found at $CONTROLLER_WASM${NC}"
        echo "Run: just build-controller"
        exit 1
    else
        echo -e "${YELLOW}Warning: Controller wasm not found at $CONTROLLER_WASM (continuing in dry-run)${NC}"
    fi
fi

if [[ -f "$TOKEN_WASM" && -f "$CONTROLLER_WASM" ]]; then
    echo -e "${GREEN}✓ Wasm files found${NC}"
fi

# Deployment configuration prompts (only if no args provided - fully interactive)
if [[ $ORIGINAL_ARGC -eq 0 ]]; then
    echo -e "\n${BLUE}Deployment Configuration:${NC}"
    read -p "Controller account ID [controller.$SIGNER_ID]: " CONTROLLER_ID
    CONTROLLER_ID=${CONTROLLER_ID:-"controller.$SIGNER_ID"}
    
    read -p "Token account ID [token.$SIGNER_ID]: " TOKEN_ID
    TOKEN_ID=${TOKEN_ID:-"token.$SIGNER_ID"}
    
    read -p "Token name [Controlled NEAR]: " TOKEN_NAME
    TOKEN_NAME=${TOKEN_NAME:-"Controlled NEAR"}
    
    read -p "Token symbol [cNEAR]: " TOKEN_SYMBOL
    TOKEN_SYMBOL=${TOKEN_SYMBOL:-"cNEAR"}
    
    read -p "Token decimals [24]: " TOKEN_DECIMALS
    TOKEN_DECIMALS=${TOKEN_DECIMALS:-24}
    
    read -p "Total supply [1000000000000000]: " TOTAL_SUPPLY
    TOTAL_SUPPLY=${TOTAL_SUPPLY:-"1000000000000000"}
else
    # Use sensible defaults for CLI mode
    CONTROLLER_ID="controller.$SIGNER_ID"
    TOKEN_ID="token.$SIGNER_ID"
    TOKEN_NAME="Controlled NEAR"
    TOKEN_SYMBOL="cNEAR"
    TOKEN_DECIMALS=24
    TOTAL_SUPPLY="1000000000000000"
fi

# Show deployment summary
echo -e "\n${BLUE}=== Deployment Summary ===${NC}"
echo "Network:           $NETWORK"
echo "Signer:            $SIGNER_ID"
echo "Controller:        $CONTROLLER_ID"
echo "Token:             $TOKEN_ID"
echo "Token Name:        $TOKEN_NAME"
echo "Token Symbol:      $TOKEN_SYMBOL"
echo "Token Decimals:    $TOKEN_DECIMALS"
echo "Total Supply:      $TOTAL_SUPPLY"
echo "Dry Run:           $DRY_RUN"
echo ""

# Construct commands
NEAR_CMD="near"
if [[ "$DRY_RUN" == "true" ]]; then
    echo -e "${YELLOW}=== DRY RUN MODE - Commands will be displayed but not executed ===${NC}\n"
fi

# Helper function to execute or print commands
run_cmd() {
    local cmd="$1"
    echo -e "${BLUE}Command:${NC} $cmd"
    if [[ "$DRY_RUN" == "false" ]]; then
        eval "$cmd"
        echo -e "${GREEN}✓ Success${NC}\n"
    else
        echo -e "${YELLOW}[DRY RUN - not executed]${NC}\n"
    fi
}

# Step 1: Deploy controller
echo -e "${GREEN}=== Step 1: Deploy Controller ===${NC}"
DEPLOY_CONTROLLER_CMD="$NEAR_CMD contract deploy $CONTROLLER_ID use-file $CONTROLLER_WASM with-init-call new json-args '{\"dao\":\"$SIGNER_ID\"}' prepaid-gas '100.0 Tgas' attached-deposit '0 NEAR' network-config $NETWORK sign-with-keychain send"

run_cmd "$DEPLOY_CONTROLLER_CMD"

# Step 2: Deploy token with signer as initial owner
echo -e "${GREEN}=== Step 2: Deploy Token with Signer as Initial Owner ===${NC}"
DEPLOY_TOKEN_CMD="$NEAR_CMD contract deploy $TOKEN_ID use-file $TOKEN_WASM with-init-call new json-args '{\"owner_id\":\"$SIGNER_ID\",\"total_supply\":\"$TOTAL_SUPPLY\",\"metadata\":{\"spec\":\"ft-1.0.0\",\"name\":\"$TOKEN_NAME\",\"symbol\":\"$TOKEN_SYMBOL\",\"decimals\":$TOKEN_DECIMALS}}' prepaid-gas '100.0 Tgas' attached-deposit '0 NEAR' network-config $NETWORK sign-with-keychain send"

run_cmd "$DEPLOY_TOKEN_CMD"

# Step 3: Transfer token ownership to controller
echo -e "${GREEN}=== Step 3: Transfer Token Ownership to Controller ===${NC}"
TRANSFER_OWNERSHIP_CMD="$NEAR_CMD contract call-function as-transaction $TOKEN_ID owner_set json-args '{\"new_owner\":\"$CONTROLLER_ID\"}' prepaid-gas '100.0 Tgas' attached-deposit '0 NEAR' sign-as $SIGNER_ID network-config $NETWORK sign-with-keychain send"

run_cmd "$TRANSFER_OWNERSHIP_CMD"

# Step 4: Verify ownership
echo -e "${GREEN}=== Step 4: Verify Ownership ===${NC}"
VERIFY_CMD="$NEAR_CMD contract call-function as-read-only $TOKEN_ID owner_get json-args {} network-config $NETWORK now"

if [[ "$DRY_RUN" == "false" ]]; then
    echo -e "${BLUE}Verifying token owner...${NC}"
    OWNER_RESULT=$(eval "$VERIFY_CMD" 2>/dev/null | grep -o '"[^"]*"' | tr -d '"' || echo "")
    
    if [[ "$OWNER_RESULT" == "$CONTROLLER_ID" ]]; then
        echo -e "${GREEN}✓ Ownership verified: $OWNER_RESULT${NC}\n"
    else
        echo -e "${RED}✗ Ownership verification failed. Expected: $CONTROLLER_ID, Got: $OWNER_RESULT${NC}\n"
    fi
else
    echo -e "${BLUE}Command:${NC} $VERIFY_CMD"
    echo -e "${YELLOW}[DRY RUN - not executed]${NC}\n"
fi

# Summary
if [[ "$DRY_RUN" == "false" ]]; then
    echo -e "${GREEN}=== Deployment Complete ===${NC}"
    echo -e "Controller: ${BLUE}$CONTROLLER_ID${NC}"
    echo -e "Token:      ${BLUE}$TOKEN_ID${NC}"
    echo -e "Owner:      ${BLUE}$CONTROLLER_ID${NC}"
    echo ""
    echo "Next steps:"
    echo "  1. Add release info to controller"
    echo "  2. Test pause/freeze via controller"
    echo "  3. Test upgrades via controller"
else
    echo -e "${YELLOW}=== Dry Run Complete - No changes made ===${NC}"
fi
