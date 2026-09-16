const { withAppDelegate } = require('@expo/config-plugins');

function modifySwiftAppDelegate(contents) {
  let next = contents;
  if (!next.includes('import react_native_paste_input')) {
    const importAnchor = 'import ReactAppDependencyProvider';
    if (!next.includes(importAnchor)) {
      throw new Error('Could not find the ReactAppDependencyProvider import in AppDelegate.swift');
    }
    next = next.replace(importAnchor, `${importAnchor}\nimport react_native_paste_input`);
  }

  if (!next.includes('PasteInputModule.setup(factory.rootViewFactory)')) {
    const startPattern = /(factory\.startReactNative\([\s\S]*?launchOptions:\s*launchOptions\)\n)/;
    if (!startPattern.test(next)) {
      throw new Error('Could not find ExpoReactNativeFactory.startReactNative in AppDelegate.swift');
    }
    next = next.replace(
      startPattern,
      '$1    PasteInputModule.setup(factory.rootViewFactory)\n',
    );
  }
  return next;
}

module.exports = function withPasteInput(config) {
  return withAppDelegate(config, (nextConfig) => {
    if (nextConfig.modResults.language !== 'swift') {
      throw new Error('@mattermost/react-native-paste-input requires a Swift AppDelegate in Waku');
    }
    nextConfig.modResults.contents = modifySwiftAppDelegate(nextConfig.modResults.contents);
    return nextConfig;
  });
};

module.exports.modifySwiftAppDelegate = modifySwiftAppDelegate;
