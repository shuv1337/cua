#include "held_phase.hpp"
#include "held_pointer_journal.hpp"
#include "foreground_route.hpp"
#include <memory>
#include <optional>
#include <vector>
#include <string>
#include <cstdlib>
#include <iostream>
using namespace cua::hyprland;
struct Surface { bool live = true; bool good() const { return live; } };
struct Seat { struct { std::shared_ptr<Surface> pointerFocus; } m_state; };
Seat seat;
Seat* g_pSeatManager = &seat;
struct eHIDCapabilityType { static constexpr int HID_INPUT_CAPABILITY_POINTER = 1; };
namespace PROTO { struct Seat { int m_currentCaps = 1; } seat_value; Seat* seat = &seat_value; }
struct Proxy {
    bool live = true;
    unsigned buttons = 0, frames = 0;
    std::vector<std::string>* events;
    std::string name;
    bool dead = false;
    HeldPointerJournal* journal = nullptr;
    bool native = true;
    bool good() const { return live; }
    const Proxy* resource() const { return live ? this : nullptr; }
    int version() const { return 5; }
    void sendButton(unsigned, unsigned, unsigned code, int state) { ++buttons; events->push_back(name + ":release"); if (journal) journal->observe(WL_POINTER_BUTTON,code,state); }
    void sendButton(unsigned, unsigned code, int state) { if (native && !PROTO::seat->m_currentCaps) return; ++buttons; events->push_back(name + ":release"); if (journal) journal->observe(WL_POINTER_BUTTON,code,state); }
    void sendFrame() { if (native && journal && !PROTO::seat->m_currentCaps) return; ++frames; events->push_back(name + ":frame"); if (journal) journal->observe(WL_POINTER_FRAME); }
    ~Proxy() { events->push_back(name + ":destroy"); }
};
struct Client { InputRoute route; bool dead = false; };
struct Drag { Client* client; };
struct Trace { void mark(const char*, unsigned) {} };
std::string refusal(std::string_view reason) { return std::string(reason); }
std::string refusal(std::string_view reason, ForegroundFailure) { return std::string(reason); }
struct Held {
    Client* client;
    HeldPhase phase;
    std::weak_ptr<Surface> root;
    unsigned button = 272;
    std::vector<std::weak_ptr<Proxy>> foreground;
    std::vector<std::shared_ptr<Proxy>> independent;
    struct ObservedPointer { std::weak_ptr<Proxy> owner; HeldPointerJournal journal; bool destroyed = false; const Proxy* resource = nullptr; };
    std::vector<ObservedPointer> observed = {};
    bool press_attempted = false;
    InputRoute route=InputRoute::primary_foreground;
};
struct Owner {
    bool held_cleanup_unconfirmed = false;
    bool held_logger = true;
    std::optional<Held> foreground_debt;
    unsigned held_button = 272;
    std::optional<Held> held;
    std::optional<Drag> drag;
    Client* lease = nullptr;
    Client* reservation = nullptr;
    std::vector<std::unique_ptr<Client>> clients;
    std::vector<std::shared_ptr<Proxy>> pointers, keyboards, touches, seats;
    Trace* trace = nullptr;
    unsigned lane = 0;
    bool foreground_started = false;
    void send(Client&, const std::string&) {}
    void finish_foreground() { foreground_started = false; }
    void release_pointer_button() { check_no_fallback(); }
    void check_no_fallback() const { if (held_button) std::abort(); }
    void leave_pointer() {
        check_no_fallback();
        for (const auto& p : held->independent) p->events->push_back(p->name + ":leave");
    }
    void leave_keyboard() {}
    void retire_grant() { lease = nullptr; }
    void relinquish_held_owner() {cleanup_held();held.reset();}
    bool held_owns_client(const Client& c) const {return held && held->client==&c;}
    static void detach_held_watchers(std::optional<Held>&) {}
    unsigned event_ms() const { return 1; }
    unsigned serial() const { return 2; }
    // PRODUCTION_CLEANUP
    // PRODUCTION_DISCONNECT_SWEEP
};
void check(bool condition, const char* message) { if (!condition) { std::cerr << message << '\n'; std::exit(1); } }
void seed_owned_press(Owner& owner,const std::shared_ptr<Proxy>& proxy,bool native=false) {
    owner.held->press_attempted=true;
    owner.held->route=owner.held->client->route;
    owner.held->observed.push_back({proxy,HeldPointerJournal{.button=owner.held->button},false,proxy->resource()});
    proxy->native=native;proxy->journal=&owner.held->observed.back().journal;
    proxy->journal->observe(WL_POINTER_BUTTON,owner.held->button,WL_POINTER_BUTTON_STATE_PRESSED);
    proxy->journal->observe(WL_POINTER_FRAME);proxy->journal->observe(WL_POINTER_FRAME);
}
int main() {
    for (bool foreground : {false, true}) {
        for (int fault = 0; fault < 6; ++fault) {
            std::vector<std::string> events;
            auto root = std::make_shared<Surface>();
            auto successor_root = std::make_shared<Surface>();
            auto original = std::make_shared<Proxy>(true, 0, 0, &events, "original");
            auto successor = std::make_shared<Proxy>(true, 0, 0, &events, "successor");
            Client client{foreground ? InputRoute::primary_foreground : InputRoute::independent};
            Owner owner;
            owner.held.emplace(Held{&client, HeldPhase{reinterpret_cast<std::uintptr_t>(&client), "handle", SourceWitness{"original-root", 7, 3, {}}, HeldPhase::Clock::now()}, root, 272, {original}, {original}});
            seat.m_state.pointerFocus = root;
            PROTO::seat->m_currentCaps = fault == 5 ? 0 : 1;
            seed_owned_press(owner,original,foreground);
            if (fault == 1) root->live = false;
            if (fault == 2) original->live = false;
            if (fault == 3) seat.m_state.pointerFocus = successor_root;
            if (fault == 4) { seat.m_state.pointerFocus.reset(); root.reset(); }
            // Source validity and lease expiry do not gate coordinate-free
            // cleanup. Only originally captured proxies may receive release.
            owner.revoke(fault == 4 ? "source_destroyed" : fault == 2 ? "disconnected" : "lease_expired", true);
            const bool confirmed = foreground ? fault == 0 : fault != 2;
            const bool attempted = foreground ? fault == 0 : confirmed;
            check(owner.held->phase.retired && owner.held->phase.cleanup_confirmed == confirmed, "wrong retirement confirmation");
            check(owner.held_cleanup_unconfirmed == (!foreground && !confirmed), "cleanup quarantine leaked across routes");
            check(original->buttons == (attempted ? 1u : 0u), "original release count");
            check(successor->buttons == 0 && successor->frames == 0, "released successor proxy");
            owner.cleanup_held();
            check(original->buttons == (attempted ? 1u : 0u), "duplicate cleanup release");
            owner.held.reset(); original.reset();
            if (confirmed) {
                check(events.size() >= 4 && events[0] == "original:release" && events[1] == "original:frame" && events[3] == "original:destroy", "destruction preceded owned release/frame");
            }
            seat.m_state.pointerFocus.reset();
            successor.reset();
        }
    }
    {
        std::vector<std::string> events;
        auto root = std::make_shared<Surface>();
        auto original = std::make_shared<Proxy>(true, 0, 0, &events, "original");
        Client client{InputRoute::independent}; Owner owner;
        owner.held.emplace(Held{&client, HeldPhase{reinterpret_cast<std::uintptr_t>(&client), "handle", SourceWitness{"original-root",7,3,{}}, HeldPhase::Clock::now()},root,272,{}, {original}});
        seed_owned_press(owner,original);owner.held_button=0; // lane-global state is not the cleanup obligation
        root.reset();
        owner.revoke("source_destroyed",false);
        owner.held.reset(); original.reset();
        check(events == std::vector<std::string>{"original:release","original:frame","original:frame","original:leave","original:destroy"}, "production revoke order did not release original before leave/destruction");
    }
    {
        std::vector<std::string> events;
        auto root=std::make_shared<Surface>();
        auto original=std::make_shared<Proxy>(true,0,0,&events,"original");
        Owner owner;
        owner.clients.push_back(std::make_unique<Client>(InputRoute::independent));
        owner.lease=owner.reservation=owner.clients.front().get();
        owner.held.emplace(Held{owner.lease,HeldPhase{reinterpret_cast<std::uintptr_t>(owner.lease),"handle",SourceWitness{"root",7,3,{}},HeldPhase::Clock::now()},root,272,{}, {original}});
        seed_owned_press(owner,original);
        owner.lease->dead=true; root.reset();
        owner.disconnect_sweep();
        check(!owner.held && owner.clients.empty() && !owner.lease && !owner.reservation,"disconnect sweep retained dangling owner or handle");
        check(original->buttons==1 && original->frames==2,"disconnect did not release original held resources before owner destruction");
        original.reset();
        check(events==std::vector<std::string>{"original:release","original:frame","original:frame","original:destroy"},"disconnect ordering mismatch");
    }
    {
        std::vector<std::string> events;
        auto original=std::make_shared<Proxy>(true,0,0,&events,"original");
        Client client{InputRoute::independent}; Owner owner;
        owner.held_button=0; // BEGIN enter failed before any owned press
        owner.held.emplace(Held{&client,HeldPhase{reinterpret_cast<std::uintptr_t>(&client),"handle",SourceWitness{"root",7,3,{}},HeldPhase::Clock::now()}, {},272,{}, {original}});
        owner.revoke("client_not_bound",true);
        check(original->buttons==0 && original->frames==0 && owner.held->phase.cleanup_confirmed,"failed BEGIN enter emitted a spurious release");
        owner.held.reset(); original.reset();
    }
    std::cout << "production held cleanup resource policy passed\n";
}
