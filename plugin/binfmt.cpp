// SPDX-License-Identifier: MIT
// Thin adapter: DNF5 owns argument parsing; the Rust backend owns environments.
#include <dnf5/iplugin.hpp>
#include <libdnf5-cli/session.hpp>
#include <libdnf5/conf/option_string.hpp>

#include <cerrno>
#include <cstring>
#include <memory>
#include <stdexcept>
#include <string>
#include <vector>
#include <unistd.h>

#ifndef BINFMT_BACKEND
#define BINFMT_BACKEND "/usr/libexec/dnf-binfmt"
#endif

namespace {
constexpr dnf5::PluginAPIVersion API{2, 0};
constexpr dnf5::PluginVersion VERSION{0, 1, 0};
std::exception_ptr last_exception;

class Operation : public dnf5::Command {
    std::string action;
    std::vector<std::unique_ptr<libdnf5::cli::session::BoolOption>> flags;
    std::vector<std::pair<std::string, libdnf5::OptionString *>> values;
    std::unique_ptr<libdnf5::cli::session::AppendStringListOption> overlays;
    std::unique_ptr<libdnf5::cli::session::StringArgumentList> arguments;

public:
    Operation(dnf5::Context & context, const std::string & name) : Command(context, name), action(name) {}
    void set_argument_parser() override {
        auto & parser = get_context().get_argument_parser();
        auto * command = get_argument_parser_command();
        command->set_description("Manage the x86-64 muvm/FEX compatibility environment");
        using namespace libdnf5::cli::session;
        flags.push_back(std::make_unique<BoolOption>(*this, "dry-run", '\0', "Print the plan without changes", false));
        flags.push_back(std::make_unique<BoolOption>(*this, "accept-no-scripts", '\0', "Accept skipped RPM scripts and triggers (experimental)", false));
        for (const auto & name : {"profile", "state-dir", "releasever", "graphics", "session-bus"}) {
            auto * value = static_cast<libdnf5::OptionString *>(parser.add_init_value(std::make_unique<libdnf5::OptionString>("")));
            auto * arg = parser.add_new_named_arg(name);
            arg->set_long_name(name);
            arg->set_has_value(true);
            arg->set_arg_value_help("VALUE");
            arg->link_value(value);
            command->register_named_arg(arg);
            values.emplace_back(name, value);
        }
        if (action == "init") {
            overlays = std::make_unique<AppendStringListOption>(*this, "overlay", '\0', "Extra EROFS image (repeatable)", "PATH");
        }
        arguments = std::make_unique<StringArgumentList>(*this, "arguments", "Packages, local RPMs, or command arguments");
    }
    void configure() override {
        get_context().set_load_system_repo(false);
        get_context().set_load_available_repos(dnf5::Context::LoadAvailableRepos::NONE);
    }
    void run() override {
        // Global host DNF flags are intentionally not forwarded. The documented
        // private profile configuration is the transaction authority.
        std::vector<std::string> args{BINFMT_BACKEND, action};
        if (flags[0]->get_value()) args.emplace_back("--dry-run");
        if (flags[1]->get_value()) args.emplace_back("--accept-no-scripts");
        for (const auto & [name, option] : values) {
            if (!option->get_value().empty()) {
                args.push_back("--" + name);
                args.push_back(option->get_value());
            }
        }
        if (overlays) for (const auto & path : overlays->get_value()) {
            args.emplace_back("--overlay");
            args.push_back(path);
        }
        args.emplace_back("--");
        for (const auto & value : arguments->get_value()) args.push_back(value);
        std::vector<char *> argv;
        for (auto & value : args) argv.push_back(value.data());
        argv.push_back(nullptr);
        // No shell interpolation. Exec also preserves backend exit codes/signals.
        execv(BINFMT_BACKEND, argv.data());
        throw std::runtime_error(std::string("Cannot execute dnf-binfmt backend: ") + std::strerror(errno));
    }
};

class BinfmtCommand : public dnf5::Command {
public:
    explicit BinfmtCommand(dnf5::Context & context) : Command(context, "binfmt") {}
    void set_parent_command() override {
        get_context().get_argument_parser().get_root_command()->register_command(get_argument_parser_command());
    }
    void set_argument_parser() override {
        get_argument_parser_command()->set_description("Experimental x86-64 RPM compatibility through muvm/FEX");
    }
    void register_subcommands() override {
        for (const auto & name : {"init", "install", "upgrade", "remove", "list", "run", "export", "doctor", "inspect"}) {
            register_subcommand(std::make_unique<Operation>(get_context(), name));
        }
    }
    void pre_configure() override { throw_missing_command(); }
};

class BinfmtPlugin : public dnf5::IPlugin {
public:
    using IPlugin::IPlugin;
    dnf5::PluginAPIVersion get_api_version() const noexcept override { return API; }
    const char * get_name() const noexcept override { return "binfmt"; }
    dnf5::PluginVersion get_version() const noexcept override { return VERSION; }
    const char * const * get_attributes() const noexcept override {
        static const char * attrs[]{"description", nullptr};
        return attrs;
    }
    const char * get_attribute(const char * name) const noexcept override {
        return std::strcmp(name, "description") == 0 ? "Rust-backed x86-64 compatibility environments" : nullptr;
    }
    void finish() noexcept override {}
    std::vector<std::unique_ptr<dnf5::Command>> create_commands() override {
        std::vector<std::unique_ptr<dnf5::Command>> result;
        result.push_back(std::make_unique<BinfmtCommand>(get_context()));
        return result;
    }
};
} // namespace

extern "C" {
dnf5::PluginAPIVersion dnf5_plugin_get_api_version() { return API; }
const char * dnf5_plugin_get_name() { return "binfmt"; }
dnf5::PluginVersion dnf5_plugin_get_version() { return VERSION; }
dnf5::IPlugin * dnf5_plugin_new_instance(dnf5::ApplicationVersion, dnf5::Context & context) {
    try { return new BinfmtPlugin(context); }
    catch (...) { last_exception = std::current_exception(); return nullptr; }
}
void dnf5_plugin_delete_instance(dnf5::IPlugin * plugin) { delete plugin; }
std::exception_ptr * dnf5_plugin_get_last_exception() { return &last_exception; }
}
