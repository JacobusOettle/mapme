# Local shortcuts. CI runs the same commands (README.md, .github/workflows/).
# Targets marked (macOS) need Xcode 16+, XcodeGen and rustup.

SHELL := bash
VENV := .venv
PY := $(VENV)/bin/python
FIX := ../fixtures/grootkloof
DD := build/dd
GPS ?= -25.75,27.45

.PHONY: help venv swift-check scripts-check core-lint core-test core-ios-check cli gate \
        xcframework xcodeproj ios-test ipa clean

help: ## list targets
	@grep -E '^[a-z-]+:.*## ' $(MAKEFILE_LIST) | awk -F':.*## ' '{printf "  %-15s %s\n", $$1, $$2}'

$(VENV)/.stamp:
	python3 -m venv $(VENV)
	$(VENV)/bin/pip install --quiet --disable-pip-version-check tree-sitter==0.26.0 tree-sitter-swift==0.7.4 pyyaml==6.0.3
	touch $@

venv: $(VENV)/.stamp ## Python venv for the Swift syntax checker

swift-check: venv ## Swift syntax check with tree-sitter (no Xcode)
	$(PY) scripts/check_swift_syntax.py ios

scripts-check: ## bash -n, shellcheck, py_compile on scripts/
	for f in scripts/*.sh; do bash -n "$$f"; done
	if command -v shellcheck >/dev/null; then shellcheck -S warning scripts/*.sh; fi
	python3 -m py_compile scripts/*.py && rm -rf scripts/__pycache__

core-lint: ## rustfmt --check and clippy -D warnings
	cd core && cargo fmt --check && cargo clippy --all-targets -- -D warnings

core-test: ## Rust tests (release) on the real Grootkloof fixture
	cd core && cargo test --release

core-ios-check: ## cargo check the core for aarch64-apple-ios
	rustup target add aarch64-apple-ios
	cd core && cargo check --lib --release --target aarch64-apple-ios

cli: ## dev CLI on the Grootkloof map -> build/cli/*.json (GPS=lat,lon)
	mkdir -p build/cli
	cd core && cargo run --release --example georef_cli -- $(FIX)/map.jpg --gps $(GPS) \
	  --dem-dir $(FIX)/dem --dpi 300 > ../build/cli/georef-contours.json
	cd core && cargo run --release --example georef_cli -- $(FIX)/map.jpg --gps $(GPS) \
	  --dem-dir $(FIX)/dem --dpi 300 --ocr $(FIX)/ocr.json > ../build/cli/georef-grid.json
	@echo "wrote build/cli/georef-contours.json build/cli/georef-grid.json"

gate: swift-check scripts-check core-lint ## the checks CI runs before using macOS minutes

xcframework: ## (macOS) Rust core -> build/MapMeCore.xcframework
	scripts/build-core-ios.sh

xcodeproj: ## (macOS) generate ios/MapMe.xcodeproj
	cd ios && xcodegen generate --spec project.yml

ios-test: xcframework xcodeproj ## (macOS) unit + UI tests on a simulator, screenshots -> build/screenshots
	rm -rf build/TestResults.xcresult build/screenshots
	udid="$$(python3 scripts/pick-simulator.py)" && \
	  xcodebuild test -project ios/MapMe.xcodeproj -scheme MapMe -destination "id=$$udid" \
	    -derivedDataPath $(DD) -resultBundlePath build/TestResults.xcresult -parallel-testing-enabled NO \
	    CODE_SIGNING_ALLOWED=NO; status=$$?; \
	  scripts/export-screenshots.sh build/TestResults.xcresult build/screenshots; exit $$status

ipa: xcframework xcodeproj ## (macOS) unsigned Release IPA -> build/MapMe-unsigned.ipa
	xcodebuild build -project ios/MapMe.xcodeproj -scheme MapMe -configuration Release \
	  -sdk iphoneos -destination "generic/platform=iOS" -derivedDataPath $(DD) \
	  CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO CODE_SIGN_IDENTITY=""
	scripts/package-ipa.sh $(DD)/Build/Products/Release-iphoneos/MapMe.app build/MapMe-unsigned.ipa

clean: ## remove build/ (keeps core/target and .venv)
	rm -rf build
