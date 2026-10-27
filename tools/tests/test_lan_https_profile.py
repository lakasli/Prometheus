"""LAN-only proxy configuration and preservation of the backend security boundary."""
import importlib.util
import pathlib
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('lan_https_profile', ROOT / 'deploy/lan_https.py')
lan = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(lan)


class LanHttpsProfileTests(unittest.TestCase):
    def test_lan_interface_port_and_peer_acl_are_explicit(self):
        config = lan.caddyfile()
        self.assertIn('https://192.168.189.191:8086 {', config)
        self.assertIn('bind 192.168.189.191', config)
        self.assertIn('@outside not remote_ip 192.168.189.0/24', config)
        self.assertIn('respond @outside "LAN access only" 403', config)
        self.assertNotIn('0.0.0.0', config)

    def test_proxy_neither_claims_other_ports_nor_installs_system_trust(self):
        config = lan.caddyfile()
        for setting in ['admin off', 'auto_https disable_redirects', 'skip_install_trust', 'protocols h1 h2', 'tls internal']:
            self.assertIn(setting, config)
        self.assertIn('reverse_proxy 127.0.0.1:8419', config)
        self.assertNotIn('header_up Host', config)

    def test_global_addresses_and_config_injection_are_rejected(self):
        for values in [dict(host='8.8.8.8', network='8.8.8.0/24'), dict(host='127.0.0.1', network='127.0.0.0/8'),
                       dict(host='192.168.190.1'), dict(port=80), dict(upstream=0), dict(root='/tmp/root\nadmin off')]:
            with self.subTest(values=values), self.assertRaises(ValueError):
                lan.caddyfile(**values)

    def test_application_keeps_loopback_and_exact_https_secure_cookie_contract(self):
        unit = lan.backend_dropin()
        self.assertIn('--listen 127.0.0.1:8419', unit)
        self.assertIn('--origin https://192.168.189.191:8086 --secure-cookie', unit)
        self.assertIn('Wants=dsh-https.service', unit)
        self.assertIn('/current/bin/dshd', unit)

    def test_proxy_has_persistent_private_storage_and_bounded_resources(self):
        unit = lan.proxy_unit('/home/campanus/apps/dsh/shared/https/bin/caddy-2.11.7')
        self.assertIn('UMask=0077', unit)
        self.assertIn('MemoryMax=192M', unit)
        self.assertIn('TasksMax=64', unit)
        self.assertIn('/shared/config/Caddyfile', unit)
        self.assertIn('/shared/https/storage', lan.caddyfile())


if __name__ == '__main__':
    unittest.main()
