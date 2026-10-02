"""Compile the actual production cleanup_held body against fake owned proxies.

No independent cleanup algorithm: the production method is extracted verbatim.
This proves resource selection/order/refusal, not native Wayland delivery.
"""
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]

class HeldCleanupPolicyTests(unittest.TestCase):
    @unittest.skipUnless(os.environ.get('CUA_WAYLAND_SERVER_SOURCE'), 'qualified libwayland source required for queue-fault extraction')
    def test_wayland_post_logger_queue_failure_never_confirms_cleanup(self):
        source=Path(os.environ['CUA_WAYLAND_SERVER_SOURCE']).read_text()
        match=re.search(r'^static void\nhandle_array\(.*?^}',source,re.M|re.S)
        self.assertIsNotNone(match)
        fixture=(ROOT/'tests/held_logger_queue_fault_fixture.cpp').read_text().replace('// PRODUCTION_HANDLE_ARRAY',match.group())
        flags=shlex.split(subprocess.check_output(['pkg-config','--cflags','--libs','wayland-server'],text=True))
        compiler=shlex.split(os.environ.get('CXX','')) or [shutil.which('clang++-18') or shutil.which('clang++') or 'c++']
        with tempfile.TemporaryDirectory(prefix='cua-held-queue-fault-') as directory:
            cpp=Path(directory)/'fault.cpp';binary=Path(directory)/'fault';cpp.write_text(fixture)
            build=subprocess.run([*compiler,'-std=c++20','-Wall','-Wextra','-Wpedantic','-Werror','-I',str(ROOT/'src'),str(cpp),'-o',str(binary),*flags],capture_output=True,text=True,timeout=60)
            self.assertEqual(build.returncode,0,build.stdout+build.stderr)
            result=subprocess.run([str(binary)],capture_output=True,text=True,timeout=10)
            self.assertEqual(result.returncode,0,result.stdout+result.stderr)

    def test_production_observer_real_wayland_resources(self):
        source=(ROOT / 'src/input_experiment.cpp').read_text()
        methods=[]
        for name in ['stop_held_logger','detach_held_watchers','held_resource_destroyed','observed_cleanup',
                     'retain_foreground_debt','foreground_cleanup_pending','held_cleanup_pending','held_event','capture_held_pointers','foreground_button','cleanup_held','held_retired_reply','native_held_phase_live']:
            match=re.search(r'^    (?:static )?(?:void|bool|std::string) '+name+r'\(.*?^    }',source,re.M|re.S)
            self.assertIsNotNone(match,name)
            methods.append(match.group())
        fixture=(ROOT / 'tests/held_pointer_journal_fixture.cpp').read_text().replace('// PRODUCTION_OBSERVER','\n'.join(methods))
        flags=shlex.split(subprocess.check_output(['pkg-config','--cflags','--libs','wayland-server'],text=True))
        compiler=shlex.split(os.environ.get('CXX','')) or [shutil.which('clang++-18') or shutil.which('clang++') or 'c++']
        with tempfile.TemporaryDirectory(prefix='cua-held-observer-') as directory:
            cpp=Path(directory)/'observer.cpp';binary=Path(directory)/'observer';cpp.write_text(fixture)
            build=subprocess.run([*compiler,'-std=c++23','-Wall','-Wextra','-Wpedantic','-Werror','-I',str(ROOT/'src'),str(cpp),'-o',str(binary),*flags],capture_output=True,text=True,timeout=60)
            self.assertEqual(build.returncode,0,build.stdout+build.stderr)
            result=subprocess.run([str(binary)],capture_output=True,text=True,timeout=10)
            self.assertEqual(result.returncode,0,result.stdout+result.stderr)

    @unittest.skipUnless(os.environ.get('CUA_HYPRLAND_SOURCE'), 'matching Hyprland source required for native-resource policy extraction')
    def test_foreground_original_surface_release_before_leave(self):
        source_root=Path(os.environ['CUA_HYPRLAND_SOURCE'])
        source=(source_root / 'src/protocols/core/Seat.cpp').read_text()
        manager=(source_root / 'src/managers/SeatManager.cpp').read_text()
        focus=manager.split('void CSeatManager::setPointerFocus(',1)[1].split('void CSeatManager::',1)[0]
        self.assertLess(focus.index('sendLeave('),focus.index('m_state.pointerFocus = surf'))
        self.assertIn('surface->m_events.destroy.listen([this] { sendLeave(); })',source)
        methods=[]
        for name in ['sendLeave','sendButton','sendFrame']:
            match=re.search(r'^void CWLPointerResource::'+name+r'\(.*?^}',source,re.M|re.S)
            self.assertIsNotNone(match)
            methods.append(match.group())
        fixture=(ROOT / 'tests/foreground_owned_release_fixture.cpp').read_text().replace('// NATIVE_RESOURCE_METHODS','\n'.join(methods))
        compiler=shlex.split(os.environ.get('CXX','')) or [shutil.which('clang++-18') or shutil.which('clang++') or 'c++']
        with tempfile.TemporaryDirectory(prefix='cua-foreground-owned-') as directory:
            cpp=Path(directory)/'policy.cpp'; binary=Path(directory)/'policy'; cpp.write_text(fixture)
            flags=shlex.split(subprocess.check_output(['pkg-config','--cflags','--libs','wayland-server'],text=True))
            build=subprocess.run([*compiler,'-std=c++20','-Wall','-Wextra','-Wpedantic','-Werror','-I',str(ROOT/'src'),str(cpp),'-o',str(binary),*flags],capture_output=True,text=True,timeout=60)
            self.assertEqual(build.returncode,0,build.stdout+build.stderr)
            result=subprocess.run([str(binary)],capture_output=True,text=True,timeout=10)
            self.assertEqual(result.returncode,0,result.stdout+result.stderr)

    def test_original_owned_resource_cleanup(self):
        source = (ROOT / 'src/input_experiment.cpp').read_text()
        methods=[]
        for name in ['cleanup_held', 'revoke']:
            match = re.search(r'^    void '+name+r'\(.*?^    }', source, re.M | re.S)
            self.assertIsNotNone(match)
            methods.append(match.group())
        fixture = (ROOT / 'tests/held_cleanup_policy_fixture.cpp').read_text().replace('// PRODUCTION_CLEANUP', '\n'.join(methods))
        step=re.search(r'^    void step\(\).*?^    }',source,re.M|re.S).group()
        sweep=step[step.index('        if (lease && lease->dead)'):]
        fixture=fixture.replace('// PRODUCTION_DISCONNECT_SWEEP','    void disconnect_sweep() {\n'+sweep)
        compiler = shlex.split(os.environ.get('CXX', '')) or [shutil.which('clang++-18') or shutil.which('clang++') or 'c++']
        with tempfile.TemporaryDirectory(prefix='cua-held-cleanup-') as directory:
            cpp = Path(directory) / 'policy.cpp'
            binary = Path(directory) / 'policy'
            cpp.write_text(fixture)
            build = subprocess.run([*compiler, '-std=c++20', '-Wall', '-Wextra', '-Wpedantic', '-Werror', '-I', str(ROOT / 'src'), str(cpp), '-o', str(binary)], capture_output=True, text=True, timeout=60)
            self.assertEqual(build.returncode, 0, build.stdout + build.stderr)
            result = subprocess.run([str(binary)], capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

if __name__ == '__main__':
    unittest.main()
