{
  description = "Logos zcash_wallet_backend: the Zcash wallet coordinator (roles, engine jobs, events, routes).";

  inputs = {
    logos-module-builder.url = "github:logos-co/logos-module-builder";
    # Dependencies follow this builder: a skewed generated ABI crashes in provider init.
    zcash_wallet_core_module = {
      url = "github:logos-co/logos-zcash-wallet-core-module";
      inputs.logos-module-builder.follows = "logos-module-builder";
    };
    zcash_node_module = {
      url = "github:logos-co/logos-zcash-node-module";
      inputs.logos-module-builder.follows = "logos-module-builder";
    };
  };

  outputs = inputs@{ self, logos-module-builder, ... }:
    let
      nixpkgs = logos-module-builder.inputs.nixpkgs;
      systems = [ "aarch64-darwin" "x86_64-darwin" "aarch64-linux" "x86_64-linux" ];
      # x86_64-windows is a cross build from x86_64-linux.
      targets = systems ++ [ "x86_64-windows" ];
      forAllSystems = f: nixpkgs.lib.genAttrs targets f;
    in
    {
      packages = forAllSystems (system:
        (logos-module-builder.lib.mkLogosModule {
          src = ./.;
          configFile = ./metadata.json;
          flakeInputs = inputs;
        }).packages.${system});
    };
}
