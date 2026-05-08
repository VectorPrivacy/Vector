# frozen_string_literal: true

require_relative "lib/mdk/version"

Gem::Specification.new do |spec|
  spec.name          = "mdk"
  spec.version       = Mdk::VERSION
  spec.authors       = ["MDK Developers"]
  spec.email         = ["dev@ipf.dev"]

  spec.summary       = "Ruby bindings for Marmot Development Kit"
  spec.description   = "Ruby bindings for Marmot Development Kit - A Rust implementation of the Marmot Protocol for secure, decentralized group messaging."
  spec.homepage      = "https://github.com/marmot-protocol/mdk"
  spec.license       = "MIT"
  spec.required_ruby_version = ">= 3.0"

  spec.metadata["homepage_uri"] = spec.homepage
  spec.metadata["source_code_uri"] = "https://github.com/marmot-protocol/mdk"
  spec.metadata["changelog_uri"] = "https://github.com/marmot-protocol/mdk"

  spec.files = Dir["lib/**/*", "ext/**/*"]
  spec.require_paths = ["lib"]

  spec.add_runtime_dependency "ffi", "~> 1.17.2"
end

