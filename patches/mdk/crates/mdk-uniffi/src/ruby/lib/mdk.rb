# frozen_string_literal: true

require_relative "mdk/version"
require_relative "mdk/mdk_uniffi"

module Mdk
  # Re-export everything from MdkUniffi module
  extend MdkUniffi
  
  # Make all MdkUniffi methods available at the Mdk level
  def self.method_missing(method_name, *args, &block)
    if MdkUniffi.respond_to?(method_name)
      MdkUniffi.send(method_name, *args, &block)
    else
      super
    end
  end
  
  def self.respond_to_missing?(method_name, include_private = false)
    MdkUniffi.respond_to?(method_name, include_private) || super
  end
end

