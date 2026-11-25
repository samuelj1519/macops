# frozen_string_literal: true
# macops-owned query helper. Data is passed only through ARGV.
require "json"
require "digest/sha2"
require "formula"
require "keg"
require "tab"
require "cli/named_args"

mode, qualified, query_id = ARGV
raise "invalid query arguments" unless ARGV.length == 3 && mode == "observe" &&
  qualified.match?(%r{\A[a-z0-9_-]+/[a-z0-9_-]+/[a-z0-9][a-z0-9+_.@-]*\z}) &&
  query_id.match?(/\A[a-zA-Z0-9][a-zA-Z0-9_-]*\z/)
requested_tap, requested_name = Tap.with_formula_name(qualified)
raise "missing exact selector" unless requested_tap && requested_name

def node(path)
  stat = path.lstat
  raise "unsupported symlink or entry" if stat.symlink?
  {"identity"=>{"device"=>stat.dev,"inode"=>stat.ino},"mode"=>stat.mode,
   "size"=>stat.size,"modified_ns"=>(stat.mtime.to_r * 1_000_000_000).to_i}
end

def keg_path(keg)
  Pathname.new(keg.to_s).realpath.to_s
end

def keg_row(keg)
  receipt = keg/AbstractTab::FILENAME
  before = node(receipt)
  raise "nonregular receipt" unless receipt.file?
  tab = keg.tab
  tap = tab.tap
  raise "unknown effective tap" unless tap
  digest = Digest::SHA256.file(receipt.to_s).hexdigest
  raise "receipt changed" unless before == node(receipt)
  {"path"=>keg_path(keg),"name"=>keg.name,"tap"=>tap.name,
   "full_name"=>(tap.core_tap? ? keg.name : "#{tap.name}/#{keg.name}"),
   "spec"=>tab.spec.to_s,"version"=>keg.version.to_s,
   "receipt"=>{"node"=>before,"sha256"=>digest}}
end

prefix = HOMEBREW_PREFIX.realpath
cellar = HOMEBREW_CELLAR.realpath
cellar_before = node(cellar)
rack = cellar/requested_name
resolved = Formulary.to_rack(qualified)
resolved = resolved.parent.realpath/resolved.basename
raise "native rack redirected" unless resolved == rack && rack.parent == cellar
entries = []; kegs = []; rack_before = nil
begin
  rack_before = node(rack)
  raise "rack not directory" unless rack.directory?
  children = rack.children.sort
  raise "rack budget" if children.length > 4096
  entries = children.map do |child|
    stamp = node(child)
    kind = child.directory? ? "directory" : child.file? ? "file" : nil
    raise "unknown entry" unless kind
    {"name"=>child.basename.to_s,"kind"=>kind,"stamp"=>stamp}
  end
  kegs = children.select(&:directory?).map { |child| Keg.new(child) }
rescue Errno::ENOENT
  raise if rack.exist? || rack.symlink?
end
rows = kegs.map { |keg| keg_row(keg) }
native_selected = begin
  Homebrew::CLI::NamedArgs.new(qualified).to_kegs_to_casks(only: :formula, method: :kegs).fetch(0)
rescue NoSuchKegError
  []
end
matching = kegs.select { |keg| keg.tab.tap == requested_tap }
raise "native selection mismatch" unless native_selected.map { |keg| keg_path(keg) }.sort == matching.map { |keg| keg_path(keg) }.sort
formula = begin
  f = Formulary.resolve(qualified)
  {"name"=>f.name,"tap"=>f.tap&.name,"full_name"=>f.full_name,
   "active_spec"=>f.active_spec_sym.to_s,"pkg_version"=>f.pkg_version.to_s,
   "pinned"=>f.pinned?,"outdated"=>f.outdated?,"resolution"=>{"alias_changed"=>f.alias_changed?,
   "migration_needed"=>f.migration_needed?},"latest_full_name"=>f.latest_formula.full_name,
   "installed_paths"=>f.installed_kegs.map { |keg| keg_path(keg) }.sort}
rescue FormulaUnavailableError
  nil
end
rack_after = begin node(rack); rescue Errno::ENOENT; nil; end
raise "rack changed" unless rack_before == rack_after
raise "cellar changed" unless cellar_before == node(cellar)
report = {"schema_version"=>1,"query_id"=>query_id,"homebrew_version"=>HOMEBREW_VERSION,
 "selected_version"=>matching.max_by(&:scheme_and_version)&.version&.to_s,
 "prefix"=>prefix.to_s,"cellar"=>cellar.to_s,
 "actual_user_config_home"=>ENV.fetch("HOMEBREW_USER_CONFIG_HOME"),
 "policies"=>{"no_auto_update"=>Homebrew::EnvConfig.no_auto_update?,
  "no_install_cleanup"=>Homebrew::EnvConfig.no_install_cleanup?,
  "no_autoremove"=>Homebrew::EnvConfig.no_autoremove?,"runtime"=>{"no_bootsnap"=>Homebrew::EnvConfig.no_bootsnap?,
  "installed_dependents_check"=>!Homebrew::EnvConfig.no_installed_dependents_check?}},
 "rack"=>{"path"=>rack.to_s,"cellar_before"=>cellar_before,"cellar_after"=>node(cellar),
  "rack_before"=>rack_before,"rack_after"=>rack_after,"entries"=>entries,"kegs"=>rows,
  "native_selected"=>native_selected.map { |keg| keg_path(keg) }.sort},"formula"=>formula}
puts JSON.generate(report)
