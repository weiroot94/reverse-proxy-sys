library tun_socks;

import 'dart:async';
import 'src/tun_socks_impl.dart';

// Singleton instance of TunSocks
TunSocks? _tunnelInstance;

// Global StreamController to manage status messages consistently
final _statusController = StreamController<String>.broadcast();

/// Exposes a stream of status messages that remains active.
Stream<String> get statusStream => _statusController.stream;

/// Starts the tunnel with the given [masterAddress] and [port].
Future<void> startTunnel(String masterAddress, int port) async {
  _tunnelInstance ??= TunSocks(
    host: masterAddress,
    port: port,
    statusController: _statusController,
  );
  await _tunnelInstance!.startTunnel();
}

/// Stops the tunnel if it is running.
void stopTunnel() {
  _tunnelInstance?.stopTunnel();
  _tunnelInstance = null;
}
