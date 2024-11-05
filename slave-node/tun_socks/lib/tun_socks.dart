library tun_socks;

import 'dart:async';
import 'src/tun_socks_impl.dart';

// Singleton instance of TunSocks
TunSocks? _tunnelInstance;

/// Starts the tunnel with the given [masterAddress] and [port].
/// Checks connection status to avoid unnecessary reconnections.
Future<void> startTunnel(String masterAddress, int port) async {
  if (_tunnelInstance == null) {
    _tunnelInstance = TunSocks(
      host: masterAddress,
      port: port,
    );
  }
  await _tunnelInstance!.startTunnel();
}

/// Stops the tunnel if it is running.
void stopTunnel() {
  _tunnelInstance?.stopTunnel();
  _tunnelInstance = null;
}
